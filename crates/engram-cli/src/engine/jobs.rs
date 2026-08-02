//! Jobs: submit, claim, run, checkpoint, finish.
//!
//! The status vocabulary is storage's ([`engram_storage::jobs`]) and the one
//! rule worth repeating here is the dangerous one: **`completed` means the
//! derived layer was fully rebuilt**. `MEMORY_CONSTRAINS_DERIVED` proves a
//! deleted memory has been compiled out by finding a `completed` compile newer
//! than the deletion, so a budget-stopped or cancelled compile written as
//! `completed` tells the store a deleted memory is gone from the derived layer
//! when it is still there. That is a privacy bug wearing a bookkeeping
//! costume, and it is why [`JobOutcome`] has no third arm that could be
//! mistaken for success.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use engram_core::events::EventKind;
use engram_storage::jobs::{JobRow, NewJob, SubmitOutcome, STATUS_COMPLETED, STATUS_FAILED};
use engram_storage::journal::EventDraft;
use serde_json::{json, Value};
use uuid::Uuid;

use super::Core;
use crate::context::AppContext;
use crate::error::CliError;

/// The kinds this build can actually run. `reflect` is deliberately absent:
/// reasoning-layer registers it in wave 5, and until then submitting one is a
/// named refusal rather than a queued row nothing will ever claim.
pub const JOB_KINDS: &[&str] = &["compile", "backfill"];

/// How long a claim is good for before another worker may steal it.
pub const LEASE: Duration = Duration::from_secs(60);
/// How often a running job renews its lease and asks whether to stop.
pub const HEARTBEAT: Duration = Duration::from_secs(20);

/// Deliberately NOT `Send`.
///
/// A runner reuses existing command code, and that code holds `SqliteStore`
/// (which owns a `rusqlite::Connection`) across its awaits. Demanding `Send`
/// here would force every runner to be rewritten around a channel before it
/// could reuse a single line of the compiler. Instead the scheduler runs jobs
/// on a current-thread runtime of its own (see [`super::scheduler`]), which is
/// where a job's own connection belongs anyway.
pub type JobFuture<'a> = Pin<Box<dyn Future<Output = Result<JobOutcome, CliError>> + 'a>>;

pub trait JobRunner: Send + Sync {
    fn kind(&self) -> &'static str;
    /// Reject a malformed submission at submit time, not three minutes later
    /// inside a worker. Returns the normalised params that get persisted.
    fn validate(&self, params: Value) -> Result<Value, CliError>;
    fn run<'a>(&'a self, jc: &'a JobCtx, params: &'a Value, cursor: &'a Value) -> JobFuture<'a>;
}

pub enum JobOutcome {
    /// Ran to the end. For compile this means the derived layer was fully
    /// rebuilt.
    Completed { stats: Value },
    /// Stopped at a checkpoint and can continue. NEVER reported as completed.
    Paused {
        cursor: Value,
        stats: Value,
        reason: PauseReason,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PauseReason {
    Cancelled,
    BudgetExhausted,
    Shutdown,
    Deadline,
}

impl PauseReason {
    pub fn as_str(self) -> &'static str {
        match self {
            PauseReason::Cancelled => "cancelled",
            PauseReason::BudgetExhausted => "budget",
            PauseReason::Shutdown => "shutdown",
            PauseReason::Deadline => "deadline",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Continue {
    Yes,
    Stop(PauseReason),
}

#[derive(Default)]
pub struct JobBudget {
    pub max_usd: Option<f64>,
    pub max_llm_calls: Option<u64>,
    spent_usd_bits: AtomicU64,
    llm_calls: AtomicU64,
}

impl JobBudget {
    pub fn spend(&self, usd: f64) {
        let prev = f64::from_bits(self.spent_usd_bits.load(Ordering::Relaxed));
        self.spent_usd_bits
            .store((prev + usd).to_bits(), Ordering::Relaxed);
        self.llm_calls.fetch_add(1, Ordering::Relaxed);
    }

    pub fn spent_usd(&self) -> f64 {
        f64::from_bits(self.spent_usd_bits.load(Ordering::Relaxed))
    }

    /// Which cap fired, by name, so the pause message can say it out loud.
    pub fn exhausted(&self) -> Option<&'static str> {
        if let Some(max) = self.max_usd {
            if self.spent_usd() >= max {
                return Some("budget_usd");
            }
        }
        if let Some(max) = self.max_llm_calls {
            if self.llm_calls.load(Ordering::Relaxed) >= max {
                return Some("max_llm_chunks");
            }
        }
        None
    }
}

/// What a runner is handed.
///
/// `ctx` is a context over a FRESH reader connection to the same database.
/// `SqliteStore` owns a `Connection` and is not `Clone`, so "the same store
/// handle" is not implementable; the rule instead is that a runner READS
/// through `ctx.store` and every mutation goes through `jc.core.write(...)`.
/// That keeps one write connection per process while letting runners reuse
/// existing command code unchanged.
pub struct JobCtx {
    pub core: Core,
    pub id: Uuid,
    pub kb: String,
    pub owner: String,
    pub budget: JobBudget,
    ctx: AppContext,
}

impl JobCtx {
    pub fn app(&self) -> &AppContext {
        &self.ctx
    }

    /// Persist a resume point AND ask whether to stop, in one round trip. A
    /// runner that never calls this is simply not resumable, which is honest
    /// as long as it never reports `Paused`.
    pub fn checkpoint(
        &self,
        cursor: Value,
        done: i64,
        total: Option<i64>,
    ) -> Result<Continue, CliError> {
        if let Some(cap) = self.budget.exhausted() {
            let _ = self
                .ctx
                .store
                .checkpoint_job(self.id, &self.owner, &cursor, done, total);
            tracing::info!(job = %self.id, cap, "job stopped at its budget cap");
            return Ok(Continue::Stop(PauseReason::BudgetExhausted));
        }
        let kept = self
            .ctx
            .store
            .checkpoint_job(self.id, &self.owner, &cursor, done, total)?;
        if !kept {
            // Our lease was stolen or the row moved on. Stop rather than
            // double-write on behalf of a job we no longer own.
            return Ok(Continue::Stop(PauseReason::Shutdown));
        }
        let beat = self.ctx.store.heartbeat_job(self.id, &self.owner, LEASE)?;
        if beat.cancel_requested {
            return Ok(Continue::Stop(PauseReason::Cancelled));
        }
        Ok(Continue::Yes)
    }
}

#[derive(Default)]
pub struct JobRegistry {
    runners: HashMap<&'static str, Arc<dyn JobRunner>>,
}

impl JobRegistry {
    pub fn with_builtin_runners() -> JobRegistry {
        let mut r = JobRegistry::default();
        r.register(Arc::new(crate::commands::compile::CompileRunner));
        r.register(Arc::new(BackfillRunner));
        r
    }

    pub fn register(&mut self, runner: Arc<dyn JobRunner>) {
        self.runners.insert(runner.kind(), runner);
    }

    pub fn get(&self, kind: &str) -> Option<Arc<dyn JobRunner>> {
        self.runners.get(kind).cloned()
    }

    pub fn kinds(&self) -> Vec<&'static str> {
        let mut k: Vec<&'static str> = self.runners.keys().copied().collect();
        k.sort_unstable();
        k
    }
}

impl Core {
    /// Submit a job. Idempotent per `(kind, kb)`: a second submit while one is
    /// live returns the live handle rather than a second row, because two
    /// agents asking for the same compile want one compile.
    pub async fn submit_job(
        &self,
        kind: String,
        kb: String,
        params: Value,
    ) -> Result<(JobRow, bool), CliError> {
        let runner = self.jobs().get(&kind).ok_or_else(|| {
            CliError::BadInput(format!(
                "unknown job kind '{kind}' (expected {})",
                self.jobs().kinds().join("|")
            ))
        })?;
        let params = runner.validate(params)?;
        let actor = self.actor();
        let kind_owned = kind.clone();
        let kb_owned = kb.clone();
        self.write("job.submit", move |w| {
            let outcome = w.store.submit_job(&NewJob {
                kind: &kind_owned,
                kb: &kb_owned,
                params: params.clone(),
                actor: &actor,
            })?;
            match outcome {
                SubmitOutcome::Submitted(id) => {
                    // Same transaction as the INSERT (invariant 1). Not
                    // appended for AlreadyLive: nothing was written.
                    w.store.append_event(
                        &EventDraft::new(
                            EventKind::JobSubmitted,
                            w.actor,
                            json!({ "job_id": id, "kind": kind_owned, "params": params }),
                        )
                        .subject(id)
                        .scope(Some(&kb_owned), None),
                    )?;
                    let row = w.store.get_job(id)?.ok_or_else(|| {
                        CliError::Transient("the job row vanished after its insert".to_string())
                    })?;
                    Ok((row, false))
                }
                SubmitOutcome::AlreadyLive(row) => Ok((*row, true)),
            }
        })
        .await
    }

    /// Claim a job and run it here, in this process, to a terminal or paused
    /// state. Used by the CLI's foreground path and by the daemon's scheduler.
    pub async fn run_job(&self, id: Uuid, owner: &str) -> Result<JobRow, CliError> {
        let row = self
            .read({
                move |s| {
                    s.get_job(id)?.ok_or(CliError::NotFound {
                        what: "job",
                        handle: format!("job:{id}"),
                    })
                }
            })
            .await?;
        let runner = self.jobs().get(&row.kind).ok_or_else(|| {
            CliError::BadInput(format!(
                "no runner registered for job kind '{}' in this build",
                row.kind
            ))
        })?;
        let owner_owned = owner.to_string();
        let claimed = self
            .read({
                let owner = owner_owned.clone();
                move |s| Ok(s.claim_job(id, &owner, LEASE)?)
            })
            .await?;
        if !claimed {
            return Err(CliError::Transient(format!(
                "job {id} is already running somewhere else"
            )));
        }

        let jc = JobCtx {
            core: self.clone(),
            id,
            kb: row.kb.clone(),
            owner: owner_owned.clone(),
            budget: budget_from_params(&row.params),
            ctx: AppContext::for_job(self, id)?,
        };
        let outcome = runner.run(&jc, &row.params, &row.cursor).await;
        let kind = row.kind.clone();
        let kb = row.kb.clone();
        self.finish(id, &owner_owned, &kind, &kb, outcome).await
    }

    async fn finish(
        &self,
        id: Uuid,
        owner: &str,
        kind: &str,
        kb: &str,
        outcome: Result<JobOutcome, CliError>,
    ) -> Result<JobRow, CliError> {
        let owner = owner.to_string();
        let kind = kind.to_string();
        let kb = kb.to_string();
        self.write("job.finish", move |w| {
            let (status, message, stats, event) = match &outcome {
                Ok(JobOutcome::Completed { stats }) => (
                    STATUS_COMPLETED.to_string(),
                    None,
                    stats.clone(),
                    Some(EventKind::JobCompleted),
                ),
                Ok(JobOutcome::Paused {
                    cursor,
                    stats,
                    reason,
                }) => {
                    // Persist the resume point outside the runner's own
                    // checkpoint, so a pause decided at the very last moment
                    // is still resumable.
                    let _ = w.store.checkpoint_job(id, &owner, cursor, 0, None);
                    let mut stats = stats.clone();
                    if let Value::Object(map) = &mut stats {
                        map.insert("stopped_reason".to_string(), json!(reason.as_str()));
                        map.insert(
                            "resume_command".to_string(),
                            json!(format!("engram jobs resume {id}")),
                        );
                    }
                    // Every pause is `paused`, cancellation included: a cancel
                    // that reached a checkpoint is resumable, and the terminal
                    // `cancelled` belongs to a job that never started.
                    // `paused` appends no event — the derived layer was not
                    // swapped, so there is nothing to journal.
                    (
                        engram_storage::jobs::STATUS_PAUSED.to_string(),
                        None,
                        stats,
                        None,
                    )
                }
                Err(e) => (
                    STATUS_FAILED.to_string(),
                    Some(e.to_string()),
                    json!({}),
                    Some(EventKind::JobFailed),
                ),
            };
            let moved = w
                .store
                .finish_job(id, &owner, &status, message.as_deref(), &stats)?;
            if moved {
                if let Some(kind_event) = event {
                    let payload = match kind_event {
                        EventKind::JobFailed => json!({
                            "job_id": id, "kind": kind,
                            "error": message.clone().unwrap_or_default(),
                        }),
                        _ => json!({ "job_id": id, "kind": kind, "stats": stats }),
                    };
                    w.store.append_event(
                        &EventDraft::new(kind_event, w.actor, payload)
                            .subject(id)
                            .scope(Some(&kb), None),
                    )?;
                }
            }
            w.store.get_job(id)?.ok_or_else(|| CliError::NotFound {
                what: "job",
                handle: format!("job:{id}"),
            })
        })
        .await
    }
}

fn budget_from_params(params: &Value) -> JobBudget {
    JobBudget {
        max_usd: params.get("budget_usd").and_then(|v| v.as_f64()),
        max_llm_calls: params.get("max_llm_chunks").and_then(|v| v.as_u64()),
        ..JobBudget::default()
    }
}

/// A lease owner that names the process, so `jobs list` on a shared store can
/// say who is holding what.
pub fn owner_tag() -> String {
    format!(
        "{}@{}:{}",
        std::process::id(),
        hostname(),
        Uuid::new_v4().simple()
    )
}

fn hostname() -> String {
    std::env::var("HOSTNAME")
        .ok()
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "local".to_string())
}

// ---------------------------------------------------------------------------
// The backfill runner
// ---------------------------------------------------------------------------

/// Re-derives facts for memories that never had an extraction pass.
///
/// It shells out to `engram facts backfill --limit N` per batch. That is the
/// sanctioned fallback, not the preferred shape: an in-process
/// `facts::backfill_batch` would share this process's writer, and this does
/// not. It is here because `facts.rs` belongs to another lane and a second
/// process per batch is a cost we can name, where an unsanctioned edit to
/// their file is not. The subprocess inherits `ENGRAM_STORE`, so a
/// `--store`-isolated daemon backfills into the isolated store.
pub struct BackfillRunner;

impl JobRunner for BackfillRunner {
    fn kind(&self) -> &'static str {
        "backfill"
    }

    fn validate(&self, params: Value) -> Result<Value, CliError> {
        let batch = params
            .get("batch")
            .and_then(|v| v.as_u64())
            .unwrap_or(25)
            .clamp(1, 500);
        Ok(json!({
            "batch": batch,
            "batches": params.get("batches").and_then(|v| v.as_u64()).unwrap_or(20),
            "diary": params.get("diary").cloned().unwrap_or(Value::Null),
        }))
    }

    fn run<'a>(&'a self, jc: &'a JobCtx, params: &'a Value, cursor: &'a Value) -> JobFuture<'a> {
        Box::pin(async move {
            let batch = params.get("batch").and_then(|v| v.as_u64()).unwrap_or(25);
            let batches = params.get("batches").and_then(|v| v.as_u64()).unwrap_or(20);
            let mut done = cursor
                .get("batches_done")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let mut processed = cursor
                .get("processed")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            while done < batches {
                let out = run_backfill_batch(&jc.kb, batch, params.get("diary"))?;
                let n = out
                    .get("data")
                    .and_then(|d| d.get("processed"))
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                processed += n;
                done += 1;
                if n == 0 {
                    return Ok(JobOutcome::Completed {
                        stats: json!({ "batches": done, "processed": processed, "exhausted": true }),
                    });
                }
                let cursor = json!({ "batches_done": done, "processed": processed });
                if let Continue::Stop(reason) =
                    jc.checkpoint(cursor.clone(), done as i64, Some(batches as i64))?
                {
                    return Ok(JobOutcome::Paused {
                        cursor,
                        stats: json!({ "batches": done, "processed": processed }),
                        reason,
                    });
                }
            }
            Ok(JobOutcome::Completed {
                stats: json!({ "batches": done, "processed": processed, "exhausted": false }),
            })
        })
    }
}

fn run_backfill_batch(kb: &str, limit: u64, diary: Option<&Value>) -> Result<Value, CliError> {
    let mut exe = std::env::current_exe()?;
    if exe.file_name().and_then(|n| n.to_str()) == Some("engramd") {
        exe.set_file_name("engram");
    }
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("--json")
        .arg("facts")
        .arg("backfill")
        .arg("--kb")
        .arg(kb)
        .arg("--limit")
        .arg(limit.to_string());
    if let Some(Value::String(d)) = diary {
        cmd.arg("--diary").arg(d);
    }
    let out = cmd.output()?;
    if !out.status.success() {
        return Err(CliError::Transient(format!(
            "facts backfill batch failed: {}",
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    Ok(serde_json::from_slice(&out.stdout).unwrap_or(Value::Null))
}

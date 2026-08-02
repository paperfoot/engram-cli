//! `engram jobs` — submit, inspect, cancel and resume background work.
//!
//! Where a job actually runs is the one decision this file makes, and it is
//! made from observable state rather than from a flag the caller has to
//! remember: if a daemon is live, the row is left `queued` and its scheduler
//! claims it; if not, the same process claims and runs it inline. `--detach`
//! with no daemon leaves the row queued and says so, loudly, in `data.warning`
//! — a queued job nothing will ever run is the kind of silence agents build
//! wrong assumptions on top of.

use crate::context::AppContext;
use crate::error::CliError;
use crate::output::{print_success, Listing, Metadata};
use crate::respond;
use engram_storage::jobs::{JobFilter, STATUS_PAUSED};
use serde_json::{json, Value};
use uuid::Uuid;

pub fn list(
    ctx: &AppContext,
    kb: String,
    all_kbs: bool,
    limit: usize,
    kind: Option<String>,
    status: Option<String>,
) -> Result<(), CliError> {
    let filter = JobFilter {
        kind,
        kb: if all_kbs { None } else { Some(kb.clone()) },
        status,
        limit,
        offset: 0,
    };
    let (rows, total) = ctx.store.list_jobs(&filter)?;
    let items: Vec<Value> = rows.iter().map(|r| r.to_json()).collect();
    let listing = Listing::page(items.len(), total, None);
    let mut meta = Metadata::default();
    meta.add("jobs", items.len());
    meta.add("kb", if all_kbs { "*" } else { kb.as_str() });
    // The legacy shape (`kb` + `jobs`) is kept alongside the IN-23 counters:
    // scripts read `jobs`, and `total` is what stops a page being mistaken for
    // a backlog.
    let mut data = json!({
        "kb": if all_kbs { "*" } else { kb.as_str() },
        "jobs": items,
    });
    if let Value::Object(obj) = &mut data {
        listing.attach(obj);
    }
    print_success(ctx.format, data, meta, |data| {
        println!("{}", serde_json::to_string_pretty(data).unwrap())
    });
    Ok(())
}

pub fn show(ctx: &AppContext, id: String) -> Result<(), CliError> {
    let id = parse_job_id(&id)?;
    // `not_found`, not `bad_input`: a well-formed id for a job that does not
    // exist is a different failure from a typo, and an agent branches on the
    // code.
    let Some(job) = ctx.store.get_job(id)? else {
        return Err(CliError::NotFound {
            what: "job",
            handle: format!("job:{id}"),
        });
    };
    let mut meta = Metadata::default();
    meta.add("job_id", id.to_string());
    meta.add("status", job.status.clone());
    print_success(ctx.format, json!({ "job": job.to_json() }), meta, |data| {
        println!("{}", serde_json::to_string_pretty(data).unwrap())
    });
    Ok(())
}

pub async fn submit(
    ctx: &AppContext,
    kind: String,
    kb: String,
    params: Option<String>,
    detach: bool,
    wait: bool,
) -> Result<(), CliError> {
    let params: Value = match params.as_deref() {
        None | Some("") => json!({}),
        Some(raw) => serde_json::from_str(raw).map_err(|e| CliError::MalformedJson {
            field: "--params",
            detail: e.to_string(),
        })?,
    };
    let (row, already_live) = ctx
        .core
        .submit_job(kind.clone(), kb.clone(), params)
        .await?;
    let daemon = crate::commands::serve::live_daemon();

    let mut data = json!({ "job": row.to_json(), "already_live": already_live });
    let left_to_someone_else = already_live || daemon.is_some();
    if left_to_someone_else {
        // Either another worker already owns this (kind, kb), or a live daemon
        // scheduler will claim it within a tick. Nothing to run here.
    } else if detach {
        let warning = "nothing will run this job until `engram serve` starts";
        data["warning"] = json!(warning);
        eprintln!("warning: {warning}");
    } else {
        let owner = crate::engine::jobs::owner_tag();
        let finished = ctx.core.run_job(row.id, &owner).await?;
        data["job"] = finished.to_json();
        if finished.status == STATUS_PAUSED {
            data["resume_command"] = json!(format!("engram jobs resume {}", finished.id));
        }
    }
    if wait && left_to_someone_else {
        let finished = wait_for_terminal(ctx, row.id).await?;
        data["job"] = finished;
    }

    let mut meta = Metadata::default();
    meta.add("job_id", row.id.to_string());
    meta.add("kind", kind);
    meta.add("kb", kb);
    print_success(ctx.format, data, meta, |data| {
        println!("{}", serde_json::to_string_pretty(data).unwrap())
    });
    Ok(())
}

pub async fn cancel(ctx: &AppContext, id: String) -> Result<(), CliError> {
    let id = parse_job_id(&id)?;
    let Some(job) = ctx.store.get_job(id)? else {
        return Err(CliError::NotFound {
            what: "job",
            handle: format!("job:{id}"),
        });
    };
    // Deliberately `bad_input` and NOT `conflict_already_resolved`: that code
    // means the facts-resolve claim race, and an agent branching on
    // `error.code` must not have adjudication and job lifecycle collapsed into
    // one value.
    if engram_storage::jobs::is_terminal(&job.status) {
        return Err(CliError::BadInput(format!(
            "job {id} is already {} — nothing to cancel",
            job.status
        )));
    }
    ctx.core
        .write("job.cancel", move |w| Ok(w.store.request_cancel(id)?))
        .await?;
    let job = ctx.store.get_job(id)?;
    print_success(
        ctx.format,
        json!({ "job": job.map(|j| j.to_json()) }),
        Metadata::default(),
        |data| println!("{}", serde_json::to_string_pretty(data).unwrap()),
    );
    Ok(())
}

pub async fn resume(ctx: &AppContext, id: String, force: bool) -> Result<(), CliError> {
    let id = parse_job_id(&id)?;
    let Some(job) = ctx.store.get_job(id)? else {
        return Err(CliError::NotFound {
            what: "job",
            handle: format!("job:{id}"),
        });
    };
    // Re-spending a caller's money silently is not acceptable. Without move
    // 3's derivation cache, resuming an LLM compile re-runs extraction on
    // chunks that were already paid for, so it refuses and names the cost.
    let llm = job
        .params
        .get("llm")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if llm && !force && !ctx.store.derivation_cache_available() {
        return Err(CliError::BadInput(format!(
            "resuming job {id} would re-run LLM extraction that was already paid for (no \
             derivation cache in this store); pass --force to accept the cost"
        )));
    }
    if !ctx
        .core
        .write("job.requeue", move |w| Ok(w.store.requeue_job(id)?))
        .await?
    {
        return Err(CliError::BadInput(format!(
            "job {id} is {} — only paused, failed or cancelled jobs resume",
            job.status
        )));
    }
    let mut data = json!({});
    if crate::commands::serve::live_daemon().is_none() {
        let owner = crate::engine::jobs::owner_tag();
        let finished = ctx.core.run_job(id, &owner).await?;
        data["job"] = finished.to_json();
    } else {
        data["job"] = ctx
            .store
            .get_job(id)?
            .map(|j| j.to_json())
            .unwrap_or(Value::Null);
    }
    print_success(ctx.format, data, Metadata::default(), |data| {
        println!("{}", serde_json::to_string_pretty(data).unwrap())
    });
    Ok(())
}

/// NDJSON on stdout, one line per change. Polls the row rather than
/// subscribing to an in-memory channel, so a CLI watching a daemon-run job
/// sees exactly what the daemon's own SSE stream emits.
pub async fn watch(ctx: &AppContext, id: String) -> Result<(), CliError> {
    let id = parse_job_id(&id)?;
    let mut last: Option<String> = None;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3600);
    loop {
        let Some(job) = ctx.store.get_job(id)? else {
            return Err(CliError::NotFound {
                what: "job",
                handle: format!("job:{id}"),
            });
        };
        let line = serde_json::to_string(&job.to_json())?;
        if last.as_deref() != Some(line.as_str()) {
            println!("{line}");
            last = Some(line);
        }
        if engram_storage::jobs::is_terminal(&job.status) || job.status == STATUS_PAUSED {
            return Ok(());
        }
        if std::time::Instant::now() > deadline {
            return Err(CliError::Transient(format!(
                "stopped watching job {id} after an hour; poll `engram jobs show {id}` instead"
            )));
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
}

async fn wait_for_terminal(ctx: &AppContext, id: Uuid) -> Result<Value, CliError> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3600);
    loop {
        if let Some(job) = ctx.store.get_job(id)? {
            if engram_storage::jobs::is_terminal(&job.status) || job.status == STATUS_PAUSED {
                return Ok(job.to_json());
            }
        }
        if std::time::Instant::now() > deadline {
            return Err(CliError::Transient(format!(
                "job {id} did not finish within an hour"
            )));
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
}

/// A job id, accepting the `job:` handle prefix as well as a bare uuid.
fn parse_job_id(raw: &str) -> Result<Uuid, CliError> {
    let trimmed = raw.strip_prefix("job:").unwrap_or(raw);
    Uuid::parse_str(trimmed).map_err(|_| {
        let mut err = CliError::BadInput(format!("'{raw}' is not a job id"));
        if let CliError::BadInput(msg) = &mut err {
            let hint = respond::resolve_command_string(&respond::retry_with_handle(
                "jobs show",
                "job:<uuid>",
                &[],
            ));
            msg.push_str(&format!(" — try `{hint}`"));
        }
        err
    })
}

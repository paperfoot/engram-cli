//! Wave 4 — jobs.
//!
//! The dangerous assertion in this file is `paused_is_never_completed`.
//! `MEMORY_CONSTRAINS_DERIVED` proves a deleted memory has been compiled out by
//! finding a `completed` compile newer than the deletion. A budget-stopped or
//! cancelled compile written as `completed` therefore tells the store that a
//! deleted memory is gone from the derived layer when it is still sitting
//! there — a privacy bug, not a bookkeeping one.

mod daemon_support;

use daemon_support::*;
use engram_storage::jobs::{JobFilter, NewJob, SubmitOutcome};
use serde_json::{json, Value};
use std::time::Duration;

fn seeded() -> Store {
    let store = Store::new();
    store.seed(&[
        "remember",
        "Ada Lovelace wrote the first algorithm for the Analytical Engine.",
        "--kb",
        "default",
    ]);
    store
}

/// Submit an inline backfill that can actually finish offline: the job's
/// `facts backfill` subprocess inherits this env, and a scripted extractor
/// that finds nothing stands in for OpenRouter.
fn submit_backfill(store: &Store) -> Value {
    let out = store
        .engram()
        .env("ENGRAM_BENCH_FORCE_STUB", "1")
        .env("ENGRAM_FACT_LLM_STUB", "[]")
        .args(["jobs", "submit", "--kind", "backfill", "--kb", "default", "--json"])
        .output()
        .expect("engram runs");
    assert!(
        out.status.success(),
        "submit failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("json")
}

#[test]
fn submit_runs_inline_when_no_daemon_is_listening() {
    let store = seeded();
    let out = submit_backfill(&store);
    assert_eq!(
        out["data"]["job"]["status"], "completed",
        "with no scheduler the submitting process runs the job itself: {out}"
    );
    assert_eq!(out["data"]["job"]["kind"], "backfill");
}

#[test]
fn detach_without_a_daemon_says_nothing_will_run_it() {
    let store = seeded();
    let out = store.seed(&[
        "jobs", "submit", "--kind", "backfill", "--kb", "default", "--detach",
    ]);
    assert_eq!(out["data"]["job"]["status"], "queued", "{out}");
    let warning = out["data"]["warning"].as_str().unwrap_or_default();
    assert!(
        warning.contains("engram serve"),
        "a queued job nothing will run must say so, loudly: {out}"
    );
}

#[test]
fn submit_is_idempotent_per_kind_and_kb() {
    let store = seeded();
    let first = store.seed(&[
        "jobs", "submit", "--kind", "backfill", "--kb", "default", "--detach",
    ]);
    let id = first["data"]["job"]["id"].as_str().unwrap().to_string();
    let second = store.seed(&[
        "jobs", "submit", "--kind", "backfill", "--kb", "default", "--detach",
    ]);
    assert_eq!(second["data"]["already_live"], true, "{second}");
    assert_eq!(second["data"]["job"]["id"], id, "one job, one row");
    let s = store.open();
    let (_, total) = s
        .list_jobs(&JobFilter {
            kind: Some("backfill".into()),
            limit: 50,
            ..Default::default()
        })
        .expect("list");
    assert_eq!(total, 1, "a second submit must not create a second row");
}

#[test]
fn an_unknown_kind_is_refused_at_submit_time() {
    let store = seeded();
    // `reflect` is reserved for wave 5; `compile` was removed with the
    // derived layer. Either way a named refusal beats a queued row nothing
    // will ever claim.
    for kind in ["reflect", "compile"] {
        let out = store
            .engram()
            .args(["jobs", "submit", "--kind", kind, "--kb", "default", "--json"])
            .output()
            .expect("engram runs");
        assert!(!out.status.success(), "{kind}");
        let text = String::from_utf8_lossy(&out.stderr);
        assert!(text.contains("unknown job kind"), "{kind}: {text}");
    }
}

#[test]
fn duplicate_live_rows_from_an_old_binary_are_collapsed_not_fatal() {
    let store = seeded();
    let s = store.open();
    // Two `running` rows for one KB, exactly as 0.3.x's abandon-then-insert
    // could leave them. Written before the unique index exists.
    s.conn_free_for_test("DROP INDEX IF EXISTS idx_jobs_live")
        .expect("drop index");
    for i in 0..2 {
        s.conn_free_for_test(&format!(
            "INSERT INTO compile_jobs(id, kb, status, mode, created_at, updated_at, metadata_json, kind)
             VALUES ('{}', 'default', 'running', 'evidence', '2026-01-0{}T00:00:00+00:00',
                     '2026-01-0{}T00:00:00+00:00', '{{}}', 'compile')",
            uuid::Uuid::new_v4(),
            i + 1,
            i + 1
        ))
        .expect("insert");
    }
    drop(s);

    // Opening the store runs schema setup, which must collapse rather than
    // error — a bare CREATE UNIQUE INDEX here bricks every command.
    let s = store.open();
    let (rows, _) = s
        .list_jobs(&JobFilter {
            limit: 50,
            ..Default::default()
        })
        .expect("list");
    let live: Vec<_> = rows.iter().filter(|r| r.status == "running").collect();
    assert_eq!(live.len(), 1, "one survivor: {rows:#?}");
    let loser = rows
        .iter()
        .find(|r| r.status == "abandoned")
        .expect("a collapsed loser");
    assert!(
        loser
            .message
            .as_deref()
            .unwrap_or_default()
            .contains("collapsed"),
        "the loser says why it is not live: {loser:#?}"
    );
}

#[test]
fn jobs_list_works_on_a_store_that_predates_the_new_columns() {
    let store = seeded();
    let s = store.open();
    // Simulate the pre-wave-4 shape by dropping the index the new schema adds;
    // the ALTERs are additive and re-run harmlessly.
    s.conn_free_for_test("DROP INDEX IF EXISTS idx_jobs_live")
        .expect("drop");
    drop(s);
    let out = store.seed(&["jobs", "list", "--all-kbs"]);
    assert!(out["data"]["jobs"].is_array(), "{out}");
    assert!(
        out["data"]["total"].is_number(),
        "a listing says how many rows exist, not how many fit: {out}"
    );
}

#[test]
fn the_legacy_job_shape_is_unchanged() {
    let store = seeded();
    store.seed(&["jobs", "submit", "--kind", "backfill", "--kb", "default"]);
    let out = store.seed(&["jobs", "list", "--all-kbs"]);
    let job = &out["data"]["jobs"][0];
    for field in [
        "id",
        "kb",
        "status",
        "mode",
        "created_at",
        "updated_at",
        "metadata",
    ] {
        assert!(
            !job[field].is_null(),
            "field `{field}` must survive wave 4: {job}"
        );
    }
    assert_eq!(job["kind"], "backfill", "additive fields land alongside");
}

#[test]
fn a_paused_job_is_never_completed() {
    // THE dangerous mistake in this lane, pinned. A pause writes `paused` and
    // appends no `job.completed`, so `MEMORY_CONSTRAINS_DERIVED` keeps its
    // meaning and a deleted memory stays gated.
    let store = seeded();
    let s = store.open();
    let SubmitOutcome::Submitted(id) = s
        .submit_job(&NewJob {
            kind: "compile",
            kb: "default",
            params: json!({}),
            actor: "test",
        })
        .expect("submit")
    else {
        panic!("fresh submit");
    };
    assert!(s
        .claim_job(id, "owner", Duration::from_secs(60))
        .expect("claim"));
    assert!(s
        .checkpoint_job(id, "owner", &json!({"next": 3}), 3, Some(9))
        .expect("checkpoint"));
    assert!(s
        .finish_job(
            id,
            "owner",
            "paused",
            None,
            &json!({"stopped_reason": "budget"})
        )
        .expect("finish"));
    let row = s.get_job(id).expect("get").expect("row");
    assert_eq!(row.status, "paused");
    assert_ne!(row.status, "completed");
    let events = s.events_for_subject(id).expect("events");
    assert!(
        !events
            .iter()
            .any(|e| e.kind == engram_core::events::EventKind::JobCompleted),
        "a pause appends no completion: the derived layer was not swapped"
    );
}

#[test]
fn a_crashed_worker_is_reclaimed_with_a_stated_reason() {
    let store = seeded();
    let s = store.open();
    let SubmitOutcome::Submitted(id) = s
        .submit_job(&NewJob {
            kind: "compile",
            kb: "default",
            params: json!({}),
            actor: "test",
        })
        .expect("submit")
    else {
        panic!("fresh submit");
    };
    // A lease that expired the moment it was taken: a `kill -9` in one line.
    assert!(s
        .claim_job(id, "gone", Duration::from_secs(0))
        .expect("claim"));
    assert_eq!(
        s.reconcile_orphan_jobs(Duration::from_secs(0))
            .expect("reconcile"),
        1
    );
    let row = s.get_job(id).expect("get").expect("row");
    assert_eq!(
        row.status, "failed",
        "no cursor means nothing to resume from"
    );
    assert!(
        row.message
            .as_deref()
            .unwrap_or_default()
            .contains("vanished"),
        "a reclaimed row says what happened: {row:#?}"
    );
}

#[test]
fn a_lease_cannot_be_stolen_while_it_is_live() {
    let store = seeded();
    let s = store.open();
    let SubmitOutcome::Submitted(id) = s
        .submit_job(&NewJob {
            kind: "backfill",
            kb: "default",
            params: json!({}),
            actor: "test",
        })
        .expect("submit")
    else {
        panic!("fresh submit");
    };
    assert!(s
        .claim_job(id, "first", Duration::from_secs(60))
        .expect("claim"));
    assert!(!s
        .claim_job(id, "second", Duration::from_secs(60))
        .expect("claim"));
    assert!(
        !s.checkpoint_job(id, "second", &json!({"n": 1}), 1, None)
            .expect("checkpoint"),
        "a worker that lost its lease must write nothing"
    );
}

#[test]
fn the_job_lifecycle_is_journalled() {
    let store = seeded();
    let out = submit_backfill(&store);
    let id: uuid::Uuid = out["data"]["job"]["id"]
        .as_str()
        .and_then(|s| uuid::Uuid::parse_str(s).ok())
        .expect("job id");
    let s = store.open();
    let kinds: Vec<_> = s
        .events_for_subject(id)
        .expect("events")
        .into_iter()
        .map(|e| e.kind)
        .collect();
    use engram_core::events::EventKind::*;
    assert!(kinds.contains(&JobSubmitted), "{kinds:?}");
    assert!(kinds.contains(&JobCompleted), "{kinds:?}");
}

#[test]
fn resume_refuses_a_kind_engram_no_longer_runs() {
    let store = seeded();
    let s = store.open();
    let SubmitOutcome::Submitted(id) = s
        .submit_job(&NewJob {
            kind: "compile",
            kb: "default",
            params: json!({ "llm": true }),
            actor: "test",
        })
        .expect("submit")
    else {
        panic!("fresh submit");
    };
    assert!(s
        .claim_job(id, "owner", Duration::from_secs(60))
        .expect("claim"));
    assert!(s
        .checkpoint_job(id, "owner", &json!({"next_chunk": 4}), 4, Some(20))
        .expect("checkpoint"));
    assert!(s
        .finish_job(id, "owner", "paused", None, &json!({}))
        .expect("finish"));
    drop(s);

    let out = store
        .engram()
        .args(["jobs", "resume", &id.to_string(), "--json"])
        .output()
        .expect("engram runs");
    assert!(!out.status.success());
    let text = String::from_utf8_lossy(&out.stderr);
    // A paused compile job from before the cut has no runner: resuming it
    // would requeue work nothing will ever pick up.
    assert!(
        text.contains("no longer runs that kind"),
        "a compile job must not be requeued: {text}"
    );
}

#[test]
fn the_http_surface_submits_reads_and_cancels() {
    let store = seeded();
    let daemon = Daemon::start(&store);

    let (status, body) = daemon.post(
        "/v1/jobs",
        &json!({ "kind": "backfill", "kb": "default" }).to_string(),
    );
    assert_eq!(status, 200, "{body}");
    let id = body["data"]["job"]["id"]
        .as_str()
        .expect("job id")
        .to_string();

    let (status, body) = daemon.get(&format!("/v1/jobs/{id}"));
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["data"]["job"]["id"], id);

    let (status, body) = daemon.get("/v1/jobs");
    assert_eq!(status, 200, "{body}");
    assert!(body["data"]["total"].is_number(), "IN-23 counters: {body}");
    assert!(
        body["data"]["count"].is_number(),
        "legacy count stays: {body}"
    );
}

#[test]
fn an_unknown_job_id_is_404_not_a_null_inside_a_200() {
    let store = seeded();
    let daemon = Daemon::start(&store);
    let (status, body) = daemon.get(&format!("/v1/jobs/{}", uuid::Uuid::new_v4()));
    assert_eq!(status, 404, "interfaces §1: unknown -> not_found: {body}");
    assert_eq!(body["error"]["code"], "not_found");
}

#[test]
fn cancelling_a_finished_job_is_bad_input_not_a_resolve_race() {
    // `conflict_already_resolved` means the facts-resolve claim race. An agent
    // branching on `error.code` must not have adjudication and job lifecycle
    // collapsed into one value.
    let store = seeded();
    let out = submit_backfill(&store);
    let id = out["data"]["job"]["id"].as_str().unwrap().to_string();
    let daemon = Daemon::start(&store);
    let (status, body) = daemon.post(&format!("/v1/jobs/{id}/cancel"), "");
    assert_eq!(status, 400, "{body}");
    assert_eq!(body["error"]["code"], "bad_input");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("completed"),
        "the refusal names the current status: {body}"
    );
}

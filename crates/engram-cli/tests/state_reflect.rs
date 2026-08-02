//! Wave 5 — `engram reflect`: the journal read back as procedural memory.
//!
//! Deterministic and offline by construction: every lesson these tests see was
//! produced by a rule in `commands/reflect.rs`, so there is no model, no key
//! and no network anywhere in this file. The proposer RULES are unit-tested
//! against hand-built events in that module; what is tested here is the
//! command — that it scans, proposes, writes at `proposed`, and can be re-run.

mod state_support;

use serde_json::{json, Value};
use state_support::*;

/// A memory corrected seconds after it was written. The `memory.written` and
/// `memory.revised` pair the `correction_within_window` rule reads.
fn seed_fresh_correction(dir: &std::path::Path, text: &str, corrected: &str) {
    let v = run_json(
        dir,
        &[
            "remember",
            text,
            "--kb",
            "personal",
            "--offline",
            "--no-extract-facts",
        ],
    );
    let id = v["data"]["id"].as_str().expect("memory id").to_string();
    run_json(dir, &["edit", &id, "--content", corrected, "--offline"]);
}

fn reflect(dir: &std::path::Path, extra: &[&str]) -> Value {
    let mut args = vec!["reflect", "--kb", "personal"];
    args.extend_from_slice(extra);
    run_json(dir, &args)
}

fn lessons(dir: &std::path::Path, extra: &[&str]) -> Vec<Value> {
    let mut args = vec!["lessons", "list", "--kb", "personal"];
    args.extend_from_slice(extra);
    run_json(dir, &args)["data"]["lessons"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

#[test]
fn reflection_produces_lessons_with_no_model_and_no_key() {
    let dir = store();
    seed_fresh_correction(dir.path(), "Ada moved to Lispon.", "Ada moved to Lisbon.");

    let v = reflect(dir.path(), &[]);
    assert!(v["data"]["events_scanned"].as_i64().unwrap() > 0, "{v}");
    assert_eq!(v["data"]["lessons_written"], json!(1), "{v}");

    let proposed = lessons(dir.path(), &["--status", "proposed"]);
    assert_eq!(proposed.len(), 1, "{proposed:?}");
    assert_eq!(proposed[0]["rule"], json!("correction_within_window"));
    assert_eq!(proposed[0]["trigger"], json!("fresh_write_correction"));
    assert_eq!(proposed[0]["source_kind"], json!("agent_extracted"));
    assert_eq!(
        proposed[0]["evidence"].as_array().unwrap().len(),
        2,
        "the write AND the correction are the evidence: {proposed:?}"
    );
}

/// A machine sentence nobody reviewed must not reach `recall` wearing the same
/// clothes as something a person said.
#[test]
fn a_proposed_lesson_has_no_recallable_twin() {
    let dir = store();
    seed_fresh_correction(dir.path(), "Ada moved to Lispon.", "Ada moved to Lisbon.");
    reflect(dir.path(), &[]);
    let proposed = lessons(dir.path(), &["--status", "proposed"]);
    assert!(proposed[0]["memory"].is_null(), "{proposed:?}");

    let v = run_json(
        dir.path(),
        &["recall", "corrected minutes after", "--profile", "offline"],
    );
    let hits = v["data"]["results"].as_array().cloned().unwrap_or_default();
    assert!(
        hits.iter().all(|h| !h["content"]
            .as_str()
            .unwrap_or("")
            .contains("Re-read what you are about to store")),
        "an unconfirmed proposal reached recall: {v}"
    );
}

/// Re-running over a window it already read writes nothing new AND does not
/// inflate the counter: the evidence is the identity of the learning event, so
/// a scheduled pass cannot make a lesson look ten times more learned than it
/// is.
#[test]
fn a_second_pass_over_the_same_window_changes_nothing() {
    let dir = store();
    seed_fresh_correction(dir.path(), "Ada moved to Lispon.", "Ada moved to Lisbon.");
    let first = reflect(dir.path(), &[]);
    let second = reflect(dir.path(), &[]);

    assert_eq!(second["data"]["lessons_written"], json!(0), "{second}");
    assert_eq!(second["data"]["lessons_merged"], json!(1));
    assert_eq!(
        first["data"]["high_water_seq"],
        second["data"]["high_water_seq"]
    );
    let rows = lessons(dir.path(), &["--status", "proposed"]);
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0]["occurrences"],
        json!(1),
        "the counter means `learned again`, not `the job ran again`: {rows:?}"
    );
}

#[test]
fn new_work_after_a_pass_is_picked_up_by_the_next_one() {
    let dir = store();
    seed_fresh_correction(dir.path(), "Ada moved to Lispon.", "Ada moved to Lisbon.");
    let first = reflect(dir.path(), &[]);
    let mark = first["data"]["high_water_seq"].as_i64().unwrap();

    seed_fresh_correction(dir.path(), "Bruno joined Acmee.", "Bruno joined Acme.");
    let second = reflect(dir.path(), &[]);
    assert!(
        second["data"]["high_water_seq"].as_i64().unwrap() > mark,
        "the second pass saw newer journal: {second}"
    );
    // Same trigger, same templated sentence shape, NEW evidence — so the
    // counter rises rather than a second near-identical row appearing.
    let rows = lessons(dir.path(), &["--status", "proposed"]);
    assert_eq!(rows.len(), 1, "{rows:?}");
}

#[test]
fn dry_run_writes_nothing_at_all() {
    let dir = store();
    seed_fresh_correction(dir.path(), "Ada moved to Lispon.", "Ada moved to Lisbon.");
    let v = reflect(dir.path(), &["--dry-run"]);
    assert!(v["data"]["candidates"].as_i64().unwrap() >= 1, "{v}");
    assert_eq!(v["data"]["lessons_written"], json!(0));
    assert_eq!(v["data"]["lessons"][0]["status"], json!("candidate"));
    assert!(lessons(dir.path(), &[]).is_empty(), "a dry run wrote a row");
    assert!(
        lessons(dir.path(), &["--status", "proposed"]).is_empty(),
        "a dry run wrote a proposal"
    );
}

/// A journal with nothing to learn from is a successful, empty answer — never
/// an error, and never an invented lesson.
#[test]
fn a_quiet_journal_teaches_nothing_and_says_so() {
    let dir = store();
    run_json(
        dir.path(),
        &[
            "remember",
            "Ada moved to Lisbon.",
            "--kb",
            "personal",
            "--offline",
            "--no-extract-facts",
        ],
    );
    let v = reflect(dir.path(), &[]);
    assert_eq!(v["data"]["candidates"], json!(0), "{v}");
    assert_eq!(v["data"]["lessons_written"], json!(0));
    assert!(lessons(dir.path(), &[]).is_empty());
}

#[test]
fn a_window_bounds_what_is_scanned() {
    let dir = store();
    for i in 0..5 {
        seed_fresh_correction(
            dir.path(),
            &format!("Note {i} with a typoo."),
            &format!("Note {i} with a typo."),
        );
    }
    let narrow = reflect(dir.path(), &["--window", "2", "--dry-run"]);
    assert!(
        narrow["data"]["events_scanned"].as_i64().unwrap() <= 2,
        "--window is a bound, not a suggestion: {narrow}"
    );
    let wide = reflect(dir.path(), &["--window", "500", "--dry-run"]);
    assert!(
        wide["data"]["events_scanned"].as_i64().unwrap()
            > narrow["data"]["events_scanned"].as_i64().unwrap()
    );
}

#[test]
fn since_takes_a_date_and_refuses_prose() {
    let dir = store();
    let ok = reflect(dir.path(), &["--since", "2026-01-01", "--dry-run"]);
    assert_eq!(ok["data"]["dry_run"], json!(true));
    let (code, err) = run_err(
        dir.path(),
        &["reflect", "--kb", "personal", "--since", "last tuesday"],
    );
    assert_eq!(code, 3, "{err}");
}

/// The single most valuable thing the journal could teach — a caller
/// overruling the machine — cannot be produced through the CLI yet, because
/// `facts resolve` still claims the queue row without appending
/// `decision.recorded`. The RULE is unit-tested in `commands/reflect.rs`
/// against hand-built events; this is the end-to-end half, gated on the same
/// ruling `wave3_fact_replay.rs` already waits on.
#[test]
#[ignore = "blocked on D8: `facts resolve` must route through resolve_conflict_with_decision so a decision.recorded event exists to learn from"]
fn a_caller_overruling_the_machine_becomes_a_lesson() {
    let dir = store();
    remember_facts(
        dir.path(),
        "personal",
        "Ada lives in Lisbon.",
        r#"[{"subject":"Ada Example","predicate":"lives_in","object":"Lisbon"}]"#,
    );
    run_json(
        dir.path(),
        &[
            "remember",
            "Ada lives in Porto.",
            "--kb",
            "personal",
            "--offline",
            "--autonomy",
            "strict",
            "--facts-json",
            r#"[{"subject":"Ada Example","predicate":"lives_in","object":"Porto"}]"#,
        ],
    );
    let conflicts = run_json(
        dir.path(),
        &["facts", "conflicts", "--open", "--kb", "personal"],
    );
    let id = conflicts["data"]["open_conflicts"][0]["conflict_id"]
        .as_str()
        .expect("an open conflict")
        .to_string();
    run_json(
        dir.path(),
        &["facts", "resolve", &id, "--action", "keep-both"],
    );

    reflect(dir.path(), &[]);
    let rows = lessons(dir.path(), &["--status", "proposed"]);
    assert!(
        rows.iter().any(|r| r["rule"] == "caller_overruled_machine"),
        "{rows:?}"
    );
}

/// The isolation gate again, on this file: every command in this suite runs
/// against `ENGRAM_STORE` on a TempDir, and nothing here reads HOME.
#[test]
fn this_suite_never_reaches_for_the_real_store() {
    // Needles assembled at runtime: a grep gate that trips on its own source
    // is a gate nobody keeps.
    let home_rewrite = format!("point{}at{}home", "_", "_");
    let env_mutation = format!("set{}var", "_");
    for src in [
        include_str!("state_reflect.rs"),
        include_str!("state_lessons.rs"),
    ] {
        assert!(
            !src.contains(&home_rewrite),
            "no HOME rewriting in this suite"
        );
        assert!(
            !src.contains(&env_mutation),
            "isolation is the harness's ENGRAM_STORE, never a test setting env vars"
        );
    }
}

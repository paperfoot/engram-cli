//! Wave 5 — `engram lessons`: procedural memory, end to end through the binary.
//!
//! What these tests hold: a lesson is one row per (kb, trigger, sentence) with
//! a counter, its twin is recallable but never becomes a world fact, a retired
//! lesson still exists, and every mutation leaves a journal event so
//! `admin rebuild` can put the projection back.

mod state_support;

use serde_json::{json, Value};
use state_support::*;

fn add(dir: &std::path::Path, lesson: &str, trigger: &str, extra: &[&str]) -> Value {
    let mut args = vec!["lessons", "add", lesson, "--trigger", trigger, "--offline"];
    args.extend_from_slice(extra);
    run_json(dir, &args)
}

fn lessons(dir: &std::path::Path, extra: &[&str]) -> Vec<Value> {
    let mut args = vec!["lessons", "list"];
    args.extend_from_slice(extra);
    run_json(dir, &args)["data"]["lessons"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

#[test]
fn a_lesson_a_human_typed_is_already_confirmed() {
    let dir = store();
    let v = add(
        dir.path(),
        "Re-read what you are about to store before storing it.",
        "fresh_write_correction",
        &[],
    );
    let data = &v["data"];
    assert_eq!(data["status"], json!("confirmed"), "{v}");
    assert_eq!(data["source_kind"], json!("user_stated"));
    let handle = data["handle"].as_str().expect("handle");
    assert!(handle.starts_with("lesson:"), "{handle}");
    assert!(uuid::Uuid::parse_str(&handle[7..]).is_ok(), "{handle}");
    // A human who typed the sentence has already confirmed it, so the twin
    // exists from the start.
    assert!(data["memory"].as_str().unwrap().starts_with("mem:"), "{v}");
}

#[test]
fn listing_narrows_by_trigger_and_reports_a_real_total() {
    let dir = store();
    add(
        dir.path(),
        "Ask before superseding.",
        "conflict_lives_in",
        &[],
    );
    add(
        dir.path(),
        "Re-read before storing.",
        "fresh_write_correction",
        &[],
    );

    let all = run_json(dir.path(), &["lessons", "list"]);
    assert_eq!(all["data"]["total"], json!(2));
    assert_eq!(all["data"]["returned"], json!(2));

    let narrowed = lessons(dir.path(), &["--trigger", "conflict_lives_in"]);
    assert_eq!(narrowed.len(), 1);
    assert_eq!(narrowed[0]["trigger"], json!("conflict_lives_in"));
}

#[test]
fn the_same_lesson_twice_is_one_row_and_one_twin() {
    let dir = store();
    let first = add(
        dir.path(),
        "Ask before superseding.",
        "conflict_lives_in",
        &[],
    );
    let second = add(
        dir.path(),
        "ask   before   superseding.",
        "conflict_lives_in",
        &[],
    );
    assert_eq!(
        first["data"]["id"], second["data"]["id"],
        "spacing and case are not a second lesson"
    );
    assert_eq!(second["data"]["occurrences"], json!(2));
    assert_eq!(lessons(dir.path(), &[]).len(), 1);
    // And the twin is not written twice: the second call merged into the row
    // that already had one.
    assert_eq!(first["data"]["memory"], second["data"]["memory"]);
}

/// The FK on `lessons.kb` fires on the very first write into any fresh KB
/// unless the KB is registered first — so the ordering inside the transaction
/// is not a detail.
#[test]
fn a_brand_new_kb_does_not_trip_the_foreign_key() {
    let dir = store();
    let v = add(
        dir.path(),
        "Check the scope before concluding a fact is missing.",
        "scope_miss",
        &["--kb", "brandnew"],
    );
    assert_eq!(v["data"]["kb"], json!("brandnew"));
    assert_eq!(lessons(dir.path(), &["--kb", "brandnew"]).len(), 1);
}

#[test]
fn a_trigger_that_is_not_a_slug_is_exit_3_and_says_the_shape() {
    let dir = store();
    let (code, err) = run_err(
        dir.path(),
        &[
            "lessons",
            "add",
            "something",
            "--trigger",
            "Not A Slug!",
            "--offline",
        ],
    );
    assert_eq!(code, 3);
    assert_eq!(err["error"]["code"], json!("bad_input"));
    assert!(
        err["error"]["message"].as_str().unwrap().contains("slug"),
        "{err}"
    );
}

#[test]
fn evidence_must_name_an_event_that_exists() {
    let dir = store();
    // A real event id, taken from the journal a write just appended.
    remember_facts(
        dir.path(),
        "personal",
        "Ada takes metformin.",
        r#"[{"subject":"Ada","predicate":"takes_medication","object":"metformin"}]"#,
    );
    let export = run_json(dir.path(), &["export", "--kb", "personal"]);
    let event_id = export["data"]["events"]
        .as_array()
        .and_then(|e| e.first())
        .and_then(|e| e["id"].as_str())
        .map(|s| s.to_string())
        .expect("the journal slice carries the events a write appended");

    let ok = add(
        dir.path(),
        "Evidence is what makes a lesson checkable.",
        "evidence_check",
        &["--evidence", &event_id],
    );
    assert_eq!(ok["data"]["evidence"].as_array().unwrap().len(), 1);
    let stamped = ok["data"]["evidence"][0].as_str().unwrap();
    assert!(stamped.starts_with("event:"), "{stamped}");

    // A uuid nothing points at is exit 3, never a lesson with a dangling
    // citation.
    let ghost = uuid::Uuid::new_v4().to_string();
    let (code, err) = run_err(
        dir.path(),
        &[
            "lessons",
            "add",
            "x",
            "--trigger",
            "t",
            "--offline",
            "--evidence",
            &ghost,
        ],
    );
    assert_eq!(code, 3, "{err}");
    assert_eq!(err["error"]["code"], json!("not_found"));

    // A handle of the wrong kind names the kind it got.
    let (code, err) = run_err(
        dir.path(),
        &[
            "lessons",
            "add",
            "x",
            "--trigger",
            "t",
            "--offline",
            "--evidence",
            &format!("mem:{ghost}"),
        ],
    );
    assert_eq!(code, 3);
    assert_eq!(err["error"]["code"], json!("wrong_id_kind"), "{err}");
}

/// The reflection layer must not poison the fact layer: a lesson ABOUT
/// medication conflicts is not a medication.
#[test]
fn a_lesson_never_becomes_a_world_fact() {
    let dir = store();
    add(
        dir.path(),
        "When a caller resolves a takes_medication conflict, keep both values.",
        "conflict_takes_medication",
        &[],
    );
    let facts = run_json(dir.path(), &["facts", "list", "--kb", "*"]);
    let rows = facts["data"]["facts"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        rows.iter().all(|f| f["predicate"] != "takes_medication"),
        "a lesson extracted a fact: {facts}"
    );
}

#[test]
fn the_twin_is_recallable_and_tagged_procedural() {
    let dir = store();
    add(
        dir.path(),
        "Ask the caller before superseding a medication slot.",
        "conflict_takes_medication",
        &[],
    );
    let v = run_json(
        dir.path(),
        &[
            "recall",
            "superseding a medication slot",
            "--profile",
            "offline",
        ],
    );
    let hits = v["data"]["results"].as_array().cloned().unwrap_or_default();
    assert!(
        !hits.is_empty(),
        "a lesson nobody can recall is a silo: {v}"
    );
    let found = hits.iter().any(|h| {
        h["content"]
            .as_str()
            .unwrap_or("")
            .contains("Ask the caller before superseding")
    });
    assert!(found, "the lesson twin is not in recall: {v}");
}

#[test]
fn retract_hides_a_lesson_without_deleting_it() {
    let dir = store();
    let v = add(
        dir.path(),
        "Ask before superseding.",
        "conflict_lives_in",
        &[],
    );
    let handle = v["data"]["handle"].as_str().unwrap().to_string();

    let retired = run_json(
        dir.path(),
        &["lessons", "retract", &handle, "--reason", "wrong"],
    );
    assert_eq!(retired["data"]["status"], json!("retired"));
    assert!(
        lessons(dir.path(), &[]).is_empty(),
        "default listing hides it"
    );
    let by_status = lessons(dir.path(), &["--status", "retired"]);
    assert_eq!(by_status.len(), 1, "the row survives");
    assert_eq!(by_status[0]["handle"], json!(handle));
}

#[test]
fn confirm_promotes_a_proposed_lesson_and_gives_it_a_twin() {
    let dir = store();
    // Only `reflect` proposes, so drive one: a memory corrected seconds after
    // it was written.
    seed_fresh_correction(dir.path());
    run_json(dir.path(), &["reflect", "--kb", "personal"]);
    let proposed = lessons(dir.path(), &["--status", "proposed"]);
    assert!(!proposed.is_empty(), "reflect proposed nothing to confirm");
    assert!(
        proposed[0]["memory"].is_null(),
        "an unreviewed machine sentence must not be recallable yet: {:?}",
        proposed[0]
    );

    let handle = proposed[0]["handle"].as_str().unwrap().to_string();
    let confirmed = run_json(dir.path(), &["lessons", "confirm", &handle, "--offline"]);
    assert_eq!(confirmed["data"]["status"], json!("confirmed"));
    assert!(
        confirmed["data"]["memory"]
            .as_str()
            .unwrap()
            .starts_with("mem:"),
        "confirmation is what makes it recallable: {confirmed}"
    );
}

#[test]
fn an_unknown_lesson_id_is_exit_3() {
    let dir = store();
    let ghost = uuid::Uuid::new_v4().to_string();
    let (code, err) = run_err(dir.path(), &["lessons", "retract", &ghost]);
    assert_eq!(code, 3);
    assert_eq!(err["error"]["code"], json!("not_found"), "{err}");
}

/// Every lesson mutation is journalled, so `admin rebuild` puts the projection
/// back exactly — the guarantee D24 exists to keep.
#[test]
fn a_rebuild_restores_every_lesson_from_the_journal_alone() {
    let dir = store();
    let v = add(
        dir.path(),
        "Ask before superseding.",
        "conflict_lives_in",
        &[],
    );
    add(
        dir.path(),
        "Ask before superseding.",
        "conflict_lives_in",
        &[],
    );
    let handle = v["data"]["handle"].as_str().unwrap().to_string();
    run_json(dir.path(), &["lessons", "retract", &handle]);

    let before = lessons(dir.path(), &["--status", "retired"]);
    let report = run_json(dir.path(), &["admin", "rebuild", "--confirm"]);
    assert!(report["data"]["events_replayed"].as_i64().unwrap_or(0) > 0);
    let after = lessons(dir.path(), &["--status", "retired"]);
    assert_eq!(before, after, "a rebuild changed a lesson");
    assert_eq!(
        after[0]["occurrences"],
        json!(2),
        "replay SETS the counter from the payload rather than counting events: {after:?}"
    );
}

/// A memory corrected moments after it was written — the one proposer trigger
/// the CLI can produce end to end today (see `state_reflect.rs` for why the
/// caller-overrules-machine path is gated on D8).
fn seed_fresh_correction(dir: &std::path::Path) {
    let v = run_json(
        dir,
        &[
            "remember",
            "Ada moved to Lispon last March.",
            "--kb",
            "personal",
            "--offline",
            "--no-extract-facts",
        ],
    );
    let id = v["data"]["id"].as_str().expect("memory id").to_string();
    run_json(
        dir,
        &[
            "edit",
            &id,
            "--content",
            "Ada moved to Lisbon last March.",
            "--offline",
        ],
    );
}

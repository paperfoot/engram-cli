//! Wave 3 — a question is asked once.
//!
//! v0.3.1 keyed a conflict by the two FACT ROWS, so a re-derivation with fresh
//! UUIDs — after `edit --content`, `facts backfill`, or a second memory saying
//! the same thing — asked the same question again. Nothing recorded WHO
//! decided, and the FK cascade that erased the queue erased the only trace.
//!
//! The key is now SEMANTIC: `(subject_norm, relation, object_pair_hash)`. Two
//! consequences are tested here — the same question is never queued twice, and
//! the key survives a re-derivation with different rows.

mod wave3_fact_support;
use wave3_fact_support::*;

const CAROL: &str = r#"[{"subject":"Carol","predicate":"works_at","object":"Acme"},
                        {"subject":"Carol","predicate":"works_at","object":"Beta Corp"}]"#;

#[test]
fn the_same_question_is_never_queued_twice() {
    let tmp = store();
    let first = remember_facts(
        tmp.path(),
        "One memo says Carol is at Acme, the next line says Beta Corp.",
        CAROL,
    );
    let queued = first["data"]["adjudication_needed"].as_array().unwrap();
    assert_eq!(queued.len(), 1, "two employers from one memo: {first}");
    let original_id = queued[0]["conflict_id"].as_str().unwrap().to_string();
    assert!(
        queued[0]["deduplicated"].is_null(),
        "the first asking is not a duplicate: {}",
        queued[0]
    );

    // A SECOND memory restates the same contradiction. New rows, new uuids,
    // the same question — and the queue must not grow.
    let second = remember_facts(
        tmp.path(),
        "A later note repeats it: Carol at Acme, then Beta Corp.",
        CAROL,
    );
    let queued = second["data"]["adjudication_needed"].as_array().unwrap();
    assert_eq!(
        open_conflicts(tmp.path())["data"]["total_open"],
        1,
        "the storm is a question asked once per re-derivation: {second}"
    );
    // Whatever it reports, it must name the question that already exists —
    // never an exit-0 success with the question silently missing.
    for entry in queued {
        assert_eq!(
            entry["conflict_id"].as_str().unwrap(),
            original_id,
            "a re-asked question must name the row already open: {entry}"
        );
        assert_eq!(
            entry["deduplicated"], true,
            "storage matched the semantic KEY, which is the only honest way to \
             claim this: {entry}"
        );
    }
}

/// The key is over the canonical relation and the two normalized objects, so a
/// synonym predicate and different casing key the same question.
#[test]
fn the_key_survives_a_re_derivation_that_spells_things_differently() {
    let tmp = store();
    let first = remember_facts(
        tmp.path(),
        "One memo says Carol is at Acme, the next line says Beta Corp.",
        CAROL,
    );
    let original_id = first["data"]["adjudication_needed"][0]["conflict_id"]
        .as_str()
        .unwrap()
        .to_string();

    let second = remember_facts(
        tmp.path(),
        "Re-read of the same memo: Carol employed by acme, then beta corp.",
        r#"[{"subject":"Carol","predicate":"employed_by","object":"acme"},
            {"subject":"Carol","predicate":"employed_by","object":"beta corp"}]"#,
    );
    assert_eq!(
        open_conflicts(tmp.path())["data"]["total_open"],
        1,
        "`employed_by` canonicalizes to `works_at`, so this is the same \
         question: {second}"
    );
    for entry in second["data"]["adjudication_needed"].as_array().unwrap() {
        assert_eq!(entry["conflict_id"].as_str().unwrap(), original_id);
    }
}

/// A coexisting pair is not a question, so a second telling of it queues
/// nothing at all — the key never even gets consulted.
#[test]
fn a_repeated_coexistence_never_becomes_a_question() {
    let tmp = store();
    let facts = r#"[{"subject":"Ana","predicate":"allergic_to","object":"penicillin"},
                    {"subject":"Ana","predicate":"allergic_to","object":"amoxicillin"}]"#;
    remember_facts(
        tmp.path(),
        "Ana is allergic to penicillin and amoxicillin.",
        facts,
    );
    remember_facts(tmp.path(), "Her chart repeats both allergies.", facts);
    assert_eq!(open_conflicts(tmp.path())["data"]["total_open"], 0);
    assert_eq!(current_facts(tmp.path(), "Ana").len(), 2);
}

/// Replay across a re-derivation, end to end: answer once, then let the same
/// pair be derived again and watch the store apply the answer instead of
/// asking.
///
/// BLOCKED, and honestly so. `facts resolve` still writes through the legacy
/// `resolve_fact_conflict` path and appends no `decision.recorded` — D8 gives
/// `resolve_conflict_with_decision` to read-surfaces in wave 3, and it has not
/// landed here. Until it does there is nothing in `fact_decisions` to replay,
/// so this asserts nothing rather than passing vacuously.
///
/// The replay SEMANTICS are proved without a store by
/// `engram_graph::semantics::tests` (`a_replayed_keep_both_suppresses_the_question`,
/// `a_caller_close_old_reapplies_silently_on_a_safety_relation`,
/// `a_machine_close_old_never_replays`), which is where they belong; what is
/// missing is one wiring, in one other lane's file.
#[test]
#[ignore = "blocked: read-surfaces must route `facts resolve` through resolve_conflict_with_decision (D8) so a decision exists to replay"]
fn a_resolved_question_is_never_asked_again() {
    let tmp = store();
    let first = remember_facts(
        tmp.path(),
        "One memo says Carol is at Acme, the next line says Beta Corp.",
        CAROL,
    );
    let conflict_id = first["data"]["adjudication_needed"][0]["conflict_id"]
        .as_str()
        .unwrap()
        .to_string();
    run_json(
        &mut engram(tmp.path()),
        &["facts", "resolve", &conflict_id, "--action", "keep-both"],
    );

    // Re-derive the same pair from a different memory. The caller answered
    // this; nobody should be asked twice.
    let again = remember_facts(
        tmp.path(),
        "Another note: Carol at Acme, then Beta Corp.",
        CAROL,
    );
    assert_eq!(
        open_conflicts(tmp.path())["data"]["total_open"],
        0,
        "a question the caller answered must not reopen: {again}"
    );
    let notes = again["data"]["plan_notes"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        notes.iter().any(|n| n["kind"] == "replay_applied"),
        "a silent replay is indistinguishable from a dropped question: {again}"
    );
}

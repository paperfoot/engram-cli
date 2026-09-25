//! Wave 3 — a fact is an interval, and an ending is not a second fact.
//!
//! v0.3.1 had no way to say a state STOPPED. "Ana stopped taking metformin"
//! either became a `stopped_taking` fact of its own — a second row that looks
//! like a second medication — or was dropped. Here the extractor's
//! `state: "ended"` and the cessation predicates close the interval that is
//! already open, and a restart opens a second one rather than resurrecting the
//! first.

mod wave3_fact_support;
use wave3_fact_support::*;

#[test]
fn an_explicit_end_closes_the_interval_and_queues_nothing() {
    let tmp = store();
    remember_facts(
        tmp.path(),
        "Ana started metformin in January 2024.",
        r#"[{"subject":"Ana","predicate":"takes_medication","object":"metformin",
             "valid_from":"2024-01-01"}]"#,
    );
    let v = remember_facts(
        tmp.path(),
        "Ana stopped taking metformin in June 2026.",
        r#"[{"subject":"Ana","predicate":"stopped_taking","object":"metformin",
             "valid_until":"2026-06-01"}]"#,
    );
    assert_eq!(
        v["data"]["adjudication_needed"].as_array().unwrap().len(),
        0,
        "an ending stated in the text is not a question: {v}"
    );
    assert_eq!(
        v["data"]["facts_added"], 0,
        "a cessation closes an interval, it does not open one: {v}"
    );

    // Nothing is current…
    assert!(
        current_facts(tmp.path(), "Ana").is_empty(),
        "a stopped medication must not read as current"
    );
    // …and the row is intact, closed at the date the text gave.
    let closed = closed_facts(tmp.path(), "Ana");
    assert_eq!(closed.len(), 1, "the history survives the ending");
    assert!(
        closed[0]["valid_until"]
            .as_str()
            .unwrap()
            .starts_with("2026-06-01"),
        "{}",
        closed[0]
    );
    assert_eq!(
        closed[0]["resolution"], "explicit_end",
        "the audit tag says the TEXT ended it, not a heuristic: {}",
        closed[0]
    );
}

/// `state: "ended"` says the same thing as a cessation predicate, and must
/// behave identically — an extractor should not have to guess which spelling
/// the store understands.
#[test]
fn the_state_field_ends_an_interval_the_same_way_a_cessation_predicate_does() {
    let tmp = store();
    remember_facts(
        tmp.path(),
        "Elias joined Acme in 2020.",
        r#"[{"subject":"Elias","predicate":"works_at","object":"Acme","valid_from":"2020-01-01"}]"#,
    );
    remember_facts(
        tmp.path(),
        "Elias left Acme in March 2026.",
        r#"[{"subject":"Elias","predicate":"works_at","object":"Acme",
             "state":"ended","valid_until":"2026-03-01"}]"#,
    );
    assert!(current_facts(tmp.path(), "Elias").is_empty());
    let closed = closed_facts(tmp.path(), "Elias");
    assert_eq!(closed.len(), 1);
    assert!(closed[0]["valid_until"]
        .as_str()
        .unwrap()
        .starts_with("2026-03-01"));
}

#[test]
fn restarting_a_medication_opens_a_second_interval() {
    let tmp = store();
    remember_facts(
        tmp.path(),
        "Ana started metformin in January 2024.",
        r#"[{"subject":"Ana","predicate":"takes_medication","object":"metformin",
             "valid_from":"2024-01-01"}]"#,
    );
    remember_facts(
        tmp.path(),
        "Ana stopped taking metformin in June 2025.",
        r#"[{"subject":"Ana","predicate":"stopped_taking","object":"metformin",
             "valid_until":"2025-06-01"}]"#,
    );
    let v = remember_facts(
        tmp.path(),
        "Ana restarted metformin in January 2026.",
        r#"[{"subject":"Ana","predicate":"takes_medication","object":"metformin",
             "valid_from":"2026-01-01"}]"#,
    );
    assert_eq!(v["data"]["facts_added"], 1, "{v}");
    assert_eq!(
        v["data"]["adjudication_needed"].as_array().unwrap().len(),
        0,
        "a restart is a second interval, not a contradiction: {v}"
    );

    assert_eq!(
        closed_facts(tmp.path(), "Ana").len(),
        1,
        "the first course stays queryable"
    );
    let current = current_facts(tmp.path(), "Ana");
    assert_eq!(current.len(), 1, "{current:?}");
    assert!(current[0]["valid_from"]
        .as_str()
        .unwrap()
        .starts_with("2026-01-01"));
}

/// The utterance DOES claim the person once took the drug, so it leaves a
/// trace — but a row born closed, and a note saying the store had nothing to
/// close, so nobody reads it as current.
#[test]
fn an_end_with_no_open_interval_is_reported_not_invented() {
    let tmp = store();
    remember_facts(
        tmp.path(),
        "Ana takes rapamycin.",
        r#"[{"subject":"Ana","predicate":"takes_medication","object":"rapamycin"}]"#,
    );
    let v = remember_facts(
        tmp.path(),
        "Ana stopped taking metformin in June 2026.",
        r#"[{"subject":"Ana","predicate":"stopped_taking","object":"metformin",
             "valid_until":"2026-06-01"}]"#,
    );
    assert_eq!(
        v["data"]["adjudication_needed"].as_array().unwrap().len(),
        0,
        "{v}"
    );
    // The rapamycin interval is untouched: stopping one drug stops one drug.
    let current = current_facts(tmp.path(), "Ana");
    assert_eq!(current.len(), 1, "{current:?}");
    assert_eq!(current[0]["object"], "rapamycin");

    let closed = closed_facts(tmp.path(), "Ana");
    assert_eq!(closed.len(), 1, "the statement leaves a trace");
    assert_eq!(closed[0]["object"], "metformin");
    assert!(closed[0]["valid_until"]
        .as_str()
        .unwrap()
        .starts_with("2026-06-01"));
}

/// Two residences that never overlapped in time are both true. Superseding one
/// loses the history the interval model exists to keep.
#[test]
fn disjoint_residences_coexist_instead_of_superseding() {
    let tmp = store();
    remember_facts(
        tmp.path(),
        "Elias lived in London from 2015 until 2020.",
        r#"[{"subject":"Elias","predicate":"lives_in","object":"London",
             "valid_from":"2015-06-01","valid_until":"2020-01-01"}]"#,
    );
    let v = remember_facts(
        tmp.path(),
        "Elias moved to Lisbon in 2024.",
        r#"[{"subject":"Elias","predicate":"lives_in","object":"Lisbon","valid_from":"2024-02-01"}]"#,
    );
    assert_eq!(
        v["data"]["conflicts"].as_array().unwrap().len(),
        0,
        "history is not a contradiction: {v}"
    );
    assert_eq!(
        v["data"]["adjudication_needed"].as_array().unwrap().len(),
        0,
        "{v}"
    );
    let current = current_facts(tmp.path(), "Elias");
    assert_eq!(current.len(), 1, "only the open interval is current");
    assert_eq!(current[0]["object"], "Lisbon");
    assert_eq!(
        closed_facts(tmp.path(), "Elias").len(),
        1,
        "London stays queryable"
    );
}

/// An overlapping single-valued update still closes the older interval, and
/// closes it at the successor's start rather than at a fresh clock read.
#[test]
fn an_overlapping_move_closes_the_old_interval_at_the_new_start() {
    let tmp = store();
    remember_facts(
        tmp.path(),
        "Elias lives in London.",
        r#"[{"subject":"Elias","predicate":"lives_in","object":"London","valid_from":"2015-06-01"}]"#,
    );
    let v = remember_facts(
        tmp.path(),
        "Elias moved to Lisbon in 2024.",
        r#"[{"subject":"Elias","predicate":"lives_in","object":"Lisbon","valid_from":"2024-02-01"}]"#,
    );
    let conflicts = v["data"]["conflicts"].as_array().unwrap();
    assert_eq!(conflicts.len(), 1, "{v}");
    assert_eq!(conflicts[0]["old_object"], "London");
    assert_eq!(conflicts[0]["new_object"], "Lisbon");

    let closed = closed_facts(tmp.path(), "Elias");
    assert_eq!(closed.len(), 1);
    assert!(
        closed[0]["valid_until"]
            .as_str()
            .unwrap()
            .starts_with("2024-02-01"),
        "the close instant is the successor's valid_from: {}",
        closed[0]
    );
}

/// An unreadable end date is dropped and REPORTED. A guessed one closes an
/// interval that is still running, which is the safety failure arriving
/// through the date parser instead of through the classifier.
#[test]
fn an_unreadable_end_date_is_reported_and_never_closes_anything() {
    let tmp = store();
    let v = remember_facts(
        tmp.path(),
        "Ana was on metformin until some time last year.",
        r#"[{"subject":"Ana","predicate":"takes_medication","object":"metformin",
             "valid_until":"some time last year"}]"#,
    );
    assert_eq!(v["data"]["facts_added"], 1, "{v}");
    let current = current_facts(tmp.path(), "Ana");
    assert_eq!(current.len(), 1);
    assert!(
        current[0]["valid_until"].is_null(),
        "a date nobody could read must not become an end date: {}",
        current[0]
    );
}

/// Two assertions about what the surfaces publish.
///
/// 1. `plan_notes` — the planner's non-fatal reports (an unreadable
///    `valid_until`, a cessation with nothing to close, a replayed decision)
///    reach the `remember` envelope instead of dying in `FactWriteOutcome`.
/// 2. the active/historic split in `facts show` follows the interval
///    (`is_open_fact()`), so a row closed with `reason: stopped` — which has no
///    successor — reads as history, not as a current medication.
#[test]
fn the_envelope_reports_a_plan_note_and_show_buckets_a_closed_interval_as_historic() {
    let tmp = store();
    let v = remember_facts(
        tmp.path(),
        "Ana was on metformin until some time last year.",
        r#"[{"subject":"Ana","predicate":"takes_medication","object":"metformin",
             "valid_until":"some time last year"}]"#,
    );
    let notes = v["data"]["plan_notes"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        notes.iter().any(|n| n["kind"] == "unreadable_valid_until"),
        "the caller is the only one who can fix the source: {v}"
    );

    remember_facts(
        tmp.path(),
        "Ana stopped taking metformin in June 2026.",
        r#"[{"subject":"Ana","predicate":"stopped_taking","object":"metformin",
             "valid_until":"2026-06-01"}]"#,
    );
    let shown = facts_show(tmp.path(), "Ana");
    assert_eq!(shown["data"]["active_count"], 0, "{shown}");
    assert_eq!(shown["data"]["historic_count"], 1, "{shown}");
}

/// `facts backfill` writes through the same planner as `remember`, so it
/// reports the same notes: a sweep that dropped an unreadable end date must
/// say so, or the operator re-runs it forever wondering why nothing closed.
#[test]
fn backfill_reports_plan_notes_too() {
    let tmp = store();
    run_json(
        &mut engram(tmp.path()),
        &[
            "remember",
            "Ana was on metformin until some time last year.",
            "--kb",
            "personal",
            "--no-extract-facts",
        ],
    );
    let v = run_json(
        engram(tmp.path()).env(
            "ENGRAM_FACT_LLM_STUB",
            r#"[{"subject":"Ana","predicate":"takes_medication","object":"metformin",
                 "valid_until":"some time last year"}]"#,
        ),
        &["facts", "backfill", "--kb", "personal"],
    );
    assert_eq!(v["data"]["facts_added"], 1, "{v}");
    let notes = v["data"]["plan_notes"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        notes.iter().any(|n| n["kind"] == "unreadable_valid_until"),
        "{v}"
    );
}

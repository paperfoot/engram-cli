//! `--autonomy full` — the machine adjudicator fails CLOSED.
//!
//! The adjudicator is a `Heuristic` authority. Whatever it answers — or fails
//! to answer — the store may end up with MORE open questions than `assisted`
//! would leave, never fewer on a safety relation, and never a close or an
//! absorb on behalf of an observation it threw away.
//!
//! Every test drives the judge through `ENGRAM_FACT_LLM_STUB` with a
//! `{"NEW fact:": <reply>}` rule (the adjudication prompt's user text starts
//! with `NEW fact:`); keys are stripped by the harness, so nothing here can
//! reach a network.

mod wave3_fact_support;
use serde_json::{json, Value};
use wave3_fact_support::*;

/// `remember --facts-json … --autonomy full` with the judge answering `reply`.
fn judged(store: &std::path::Path, content: &str, facts_json: &str, reply: &str) -> Value {
    let script = json!({ "NEW fact:": reply, "*": "[]" }).to_string();
    run_json(
        engram(store).env("ENGRAM_FACT_LLM_STUB", script),
        &[
            "remember",
            content,
            "--kb",
            "personal",
            "--autonomy",
            "full",
            "--facts-json",
            facts_json,
        ],
    )
}

fn med(object: &str) -> String {
    format!(r#"[{{"subject":"Ana","predicate":"takes_medication","object":"{object}"}}]"#)
}

fn birthday(object: &str) -> String {
    format!(r#"[{{"subject":"Ana","predicate":"birthday","object":"{object}"}}]"#)
}

fn queued(v: &Value) -> Vec<Value> {
    v["data"]["adjudication_needed"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

fn note_kinds(v: &Value) -> Vec<String> {
    v["data"]["plan_notes"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|n| n["kind"].as_str().map(str::to_string))
        .collect()
}

fn open_objects(store: &std::path::Path) -> Vec<String> {
    let mut objects: Vec<String> = facts_for(store, "Ana")
        .iter()
        .filter(|f| f["valid_until"].is_null())
        .filter_map(|f| f["object"].as_str().map(str::to_string))
        .collect();
    objects.sort();
    objects
}

// ----- B1: no verdict is not "coexist" -------------------------------------

#[test]
fn a_garbled_verdict_leaves_the_safety_question_queued() {
    let tmp = store();
    remember_facts(
        tmp.path(),
        "Ana takes metformin 500 mg.",
        &med("metformin 500 mg"),
    );
    let v = judged(
        tmp.path(),
        "Ana now takes metformin 1000 mg.",
        &med("metformin 1000 mg"),
        "I cannot decide",
    );
    assert_eq!(v["data"]["facts_added"], 1, "{v}");
    assert_eq!(queued(&v).len(), 1, "the question must survive: {v}");
    assert!(
        note_kinds(&v).contains(&"adjudication_failed".to_string()),
        "the envelope must say nobody decided: {v}"
    );
    assert_eq!(open_conflicts(tmp.path())["data"]["total_open"], 1);
    assert_eq!(
        open_objects(tmp.path()),
        ["metformin 1000 mg", "metformin 500 mg"]
    );
}

/// A provider's `content: null` arrives as an empty string.
#[test]
fn an_empty_reply_leaves_the_question_queued() {
    let tmp = store();
    remember_facts(
        tmp.path(),
        "Ana takes metformin 500 mg.",
        &med("metformin 500 mg"),
    );
    let v = judged(
        tmp.path(),
        "Ana now takes metformin 1000 mg.",
        &med("metformin 1000 mg"),
        "",
    );
    assert_eq!(queued(&v).len(), 1, "{v}");
    assert!(note_kinds(&v).contains(&"adjudication_failed".to_string()));
}

/// Not only safety relations: a non-verdict decides nothing anywhere.
#[test]
fn a_non_verdict_decides_nothing_on_a_routine_relation_either() {
    let tmp = store();
    remember_facts(
        tmp.path(),
        "Ana was born 1990-04-12.",
        &birthday("1990-04-12"),
    );
    for (object, reply) in [
        ("1990-04-13", r#"{"action":"supersede","ids":[]}"#),
        ("1990-04-14", r#"{"action":"merge"}"#),
    ] {
        let v = judged(
            tmp.path(),
            &format!("Ana was born {object}."),
            &birthday(object),
            reply,
        );
        assert!(
            note_kinds(&v).contains(&"adjudication_failed".to_string()),
            "{reply}: {v}"
        );
        assert!(!queued(&v).is_empty(), "{reply}: {v}");
    }
    // 13 vs 12, then 14 vs 12 and 14 vs 13: every question still open.
    assert_eq!(open_conflicts(tmp.path())["data"]["total_open"], 3);
    assert_eq!(
        open_objects(tmp.path()),
        ["1990-04-12", "1990-04-13", "1990-04-14"]
    );
}

/// No key and no stub: `full` has no judge, so it queues exactly like
/// `assisted` — a missing key is not a verdict either.
#[test]
fn full_autonomy_without_a_judge_queues_like_assisted() {
    let tmp = store();
    remember_facts(
        tmp.path(),
        "Ana takes metformin 500 mg.",
        &med("metformin 500 mg"),
    );
    let v = run_json(
        &mut engram(tmp.path()),
        &[
            "remember",
            "Ana now takes metformin 1000 mg.",
            "--kb",
            "personal",
            "--autonomy",
            "full",
            "--facts-json",
            &med("metformin 1000 mg"),
        ],
    );
    assert_eq!(queued(&v).len(), 1, "{v}");
    assert_eq!(queued(&v)[0]["suggested_action"], "requires_context");
}

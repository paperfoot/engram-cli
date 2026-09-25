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

// ----- B2: reject_new discards the WHOLE observation -----------------------

/// The report's repro: a caller's birthday correction plans a close of the
/// extractor's guess; the judge then rejects the correction. The close used to
/// survive — `close_authority=caller`, `closed_by` naming a fact that was never
/// written.
#[test]
fn a_rejected_observation_closes_nothing_on_its_way_out() {
    let tmp = store();
    remember_facts(
        tmp.path(),
        "Ana was born 1990-04-12.",
        &birthday("1990-04-12"),
    );
    // An EXTRACTED 04-13, so the caller's 04-14 below is a correction of it.
    run_json(
        engram(tmp.path()).env(
            "ENGRAM_FACT_LLM_STUB",
            r#"[{"subject":"Ana","predicate":"birthday","object":"1990-04-13"}]"#,
        ),
        &[
            "remember",
            "Ana's birthday is the 13th.",
            "--kb",
            "personal",
        ],
    );
    let v = judged(
        tmp.path(),
        "Ana was born 1990-04-14.",
        &birthday("1990-04-14"),
        r#"{"action":"reject_new"}"#,
    );
    assert_eq!(v["data"]["facts_added"], 0, "{v}");
    assert_eq!(v["data"]["facts_deduped"], 1, "{v}");
    assert_eq!(
        v["data"]["conflicts"].as_array().unwrap().len(),
        0,
        "a discarded observation closes nothing: {v}"
    );
    assert!(
        closed_facts(tmp.path(), "Ana").is_empty(),
        "no interval may be closed by a fact that does not exist"
    );
    assert_eq!(open_objects(tmp.path()), ["1990-04-12", "1990-04-13"]);
}

// ----- B3: a safety question is the caller's -------------------------------

/// The report's repro: a caller-asserted dose change, `reject_new`ed by the
/// judge, vanished and was reported as a duplicate.
#[test]
fn the_judge_cannot_discard_a_safety_observation() {
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
        r#"{"action":"reject_new"}"#,
    );
    assert_eq!(
        v["data"]["facts_added"], 1,
        "the caller's dose is kept: {v}"
    );
    assert_eq!(v["data"]["facts_deduped"], 0, "{v}");
    assert_eq!(queued(&v).len(), 1, "{v}");
    assert_eq!(queued(&v)[0]["suggested_action"], "requires_context");
    assert!(note_kinds(&v).contains(&"adjudication_kept_safety".to_string()));
    assert_eq!(
        open_objects(tmp.path()),
        ["metformin 1000 mg", "metformin 500 mg"]
    );
}

#[test]
fn the_judge_cannot_dismiss_a_safety_question() {
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
        r#"{"action":"coexist"}"#,
    );
    assert_eq!(queued(&v).len(), 1, "the question survives: {v}");
    assert!(note_kinds(&v).contains(&"adjudication_kept_safety".to_string()));
    assert_eq!(open_conflicts(tmp.path())["data"]["total_open"], 1);
}

/// And outside the safety registry the judge's answers still stand — the fix
/// is scoped, not a switch that turns `--autonomy full` into `assisted`.
#[test]
fn the_judge_still_settles_a_routine_question() {
    let tmp = store();
    remember_facts(
        tmp.path(),
        "Ana was born 1990-04-12.",
        &birthday("1990-04-12"),
    );
    let v = judged(
        tmp.path(),
        "Ana was born 1990-04-13.",
        &birthday("1990-04-13"),
        r#"{"action":"coexist"}"#,
    );
    assert_eq!(queued(&v).len(), 0, "{v}");
    assert!(note_kinds(&v).is_empty(), "{v}");

    let v = judged(
        tmp.path(),
        "Ana was born 1990-04-14.",
        &birthday("1990-04-14"),
        r#"{"action":"reject_new"}"#,
    );
    assert_eq!(v["data"]["facts_added"], 0, "{v}");
    assert_eq!(v["data"]["facts_deduped"], 1, "{v}");
    assert_eq!(open_objects(tmp.path()), ["1990-04-12", "1990-04-13"]);
}

// ----- B11: a supersede that names one pair leaves the others asked --------

#[test]
fn pairs_the_judge_did_not_name_stay_queued() {
    let tmp = store();
    remember_facts(
        tmp.path(),
        "Ana was born 1990-04-12.",
        &birthday("1990-04-12"),
    );
    remember_facts(
        tmp.path(),
        "Ana was born 1990-04-13.",
        &birthday("1990-04-13"),
    );
    let id_of = |object: &str| {
        facts_for(tmp.path(), "Ana")
            .into_iter()
            .find(|f| f["object"] == object)
            .and_then(|f| f["id"].as_str().map(str::to_string))
            .expect("stored")
    };
    let first = id_of("1990-04-12");
    let v = judged(
        tmp.path(),
        "Ana was born 1990-04-14.",
        &birthday("1990-04-14"),
        &format!(r#"{{"action":"supersede","ids":["{first}"]}}"#),
    );
    assert_eq!(v["data"]["conflicts"].as_array().unwrap().len(), 1, "{v}");
    let asked: Vec<Value> = queued(&v);
    assert_eq!(
        asked.len(),
        1,
        "the unnamed 04-13 pair is still a question: {v}"
    );
    assert_eq!(asked[0]["old"]["object"], "1990-04-13");
    assert_eq!(open_objects(tmp.path()), ["1990-04-13", "1990-04-14"]);
}

// ----- B5: judged outside the write lock, applied only to what it saw ------

/// Two facts in one batch. The judge sees both BEFORE the transaction, when
/// 04-14 conflicts only with 04-12. By the time 04-14 is written, 04-13 has
/// landed and it is a second question — so the verdict answers a question
/// nobody is asking any more, and both pairs stay queued.
#[test]
fn a_verdict_about_different_pairs_is_not_applied() {
    let tmp = store();
    remember_facts(
        tmp.path(),
        "Ana was born 1990-04-12.",
        &birthday("1990-04-12"),
    );
    let v = judged(
        tmp.path(),
        "Ana was born on the 13th, or the 14th.",
        r#"[{"subject":"Ana","predicate":"birthday","object":"1990-04-13"},
            {"subject":"Ana","predicate":"birthday","object":"1990-04-14"}]"#,
        r#"{"action":"coexist"}"#,
    );
    assert!(
        note_kinds(&v).contains(&"adjudication_stale".to_string()),
        "{v}"
    );
    let asked = queued(&v);
    assert_eq!(asked.len(), 2, "{v}");
    assert!(
        asked.iter().all(|q| q["new"]["object"] == "1990-04-14"),
        "the 04-13 verdict still applied to the pairs it was about: {v}"
    );
}

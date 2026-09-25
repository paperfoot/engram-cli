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
    // The unused verdict is still journaled, as stale, on the pair it judged.
    let judged_pair = asked
        .iter()
        .find(|q| q["old"]["object"] == "1990-04-12")
        .expect("14 vs 12 queued");
    let ledger = engram_storage::SqliteStore::open(tmp.path().join("db.sqlite"))
        .expect("the temp store opens");
    let rows = ledger
        .decisions_for_key("personal", judged_pair["decision_key"].as_str().unwrap())
        .expect("history");
    assert_eq!(rows.len(), 1);
    assert!(rows[0]
        .note
        .as_deref()
        .unwrap_or_default()
        .starts_with("action=stale model="));
}

// ----- which judge answered: journaled, never binding ----------------------

/// A routine question the judge settled. Its answer is journaled — `void`,
/// `machine`, `judge:llm` — and a later write of the same question (same kb,
/// so the same semantic key, from another diary) is planned exactly as in a
/// store that never met the judge: asked again, nothing replayed. A machine
/// `keep_both` here would have answered it silently forever.
#[test]
fn a_judges_answer_is_journaled_and_binds_nothing() {
    let later = |store: &std::path::Path| {
        for object in ["1990-04-12", "1990-04-13"] {
            let v = run_json(
                &mut engram(store),
                &[
                    "remember",
                    &format!("Ana was born {object}."),
                    "--kb",
                    "personal",
                    "--diary",
                    "other",
                    "--facts-json",
                    &birthday(object),
                ],
            );
            if object == "1990-04-13" {
                return v;
            }
        }
        unreachable!()
    };

    let judged_store = store();
    remember_facts(
        judged_store.path(),
        "Ana was born 1990-04-12.",
        &birthday("1990-04-12"),
    );
    let v = judged(
        judged_store.path(),
        "Ana was born 1990-04-13.",
        &birthday("1990-04-13"),
        r#"{"action":"reject_new"}"#,
    );
    assert_eq!(v["data"]["facts_deduped"], 1, "{v}");
    let after_judge = later(judged_store.path());

    let control = store();
    let without_judge = later(control.path());

    for v in [&after_judge, &without_judge] {
        assert_eq!(queued(v).len(), 1, "asked again: {v}");
        assert!(note_kinds(v).is_empty(), "nothing replayed: {v}");
    }
    let (a, b) = (&queued(&after_judge)[0], &queued(&without_judge)[0]);
    for field in ["rule", "suggested_action", "decision_key"] {
        assert_eq!(a[field], b[field], "{field}");
    }

    let key = a["decision_key"].as_str().expect("keyed");
    let ledger = engram_storage::SqliteStore::open(judged_store.path().join("db.sqlite"))
        .expect("the temp store opens");
    let rows = ledger.decisions_for_key("personal", key).expect("history");
    assert_eq!(rows.len(), 1, "one row per judged pair");
    let row = &rows[0];
    assert_eq!(row.verdict, "void");
    assert_eq!(row.authority, "machine");
    assert_eq!(row.actor, "judge:llm");
    let note = row.note.as_deref().unwrap_or_default();
    assert!(
        note.starts_with("action=reject_new model=")
            && note.ends_with(" rule=immutable_attribute_disagreement"),
        "{note}"
    );
}

/// A failure is an answer too, as far as the journal is concerned: the pairs
/// stay queued AND the ledger says which judge failed to decide them.
#[test]
fn a_failed_judgement_is_journaled_as_failed() {
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
    let key = queued(&v)[0]["decision_key"]
        .as_str()
        .expect("keyed")
        .to_string();
    let ledger = engram_storage::SqliteStore::open(tmp.path().join("db.sqlite"))
        .expect("the temp store opens");
    let rows = ledger.decisions_for_key("personal", &key).expect("history");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].verdict, "void");
    assert!(rows[0]
        .note
        .as_deref()
        .unwrap_or_default()
        .starts_with("action=failed model="));
}

// ----- measurement: what the gate and the judge did ------------------------

#[test]
fn the_metadata_counts_what_the_gate_and_the_judge_did() {
    let tmp = store();
    let first = remember_facts(
        tmp.path(),
        "Ana was born 1990-04-12.",
        &birthday("1990-04-12"),
    );
    assert_eq!(
        first["metadata"]["gate"],
        json!({"ran": 1, "skipped_has_siblings": 0, "duplicate": 0, "novel": 1,
               "uncertain": 0, "uncertain_candidates": 0}),
        "{first}"
    );
    assert_eq!(first["metadata"]["adjudication"]["calls"], 0);

    let old = facts_for(tmp.path(), "Ana")[0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let v = judged(
        tmp.path(),
        "Ana was born 1990-04-13.",
        &birthday("1990-04-13"),
        &format!(r#"{{"action":"supersede","ids":["{old}"]}}"#),
    );
    assert_eq!(v["metadata"]["gate"]["skipped_has_siblings"], 1, "{v}");
    assert_eq!(
        v["metadata"]["adjudication"],
        json!({"calls": 1, "failed": 0, "supersede": 1, "coexist": 0, "reject_new": 0,
               "stale": 0, "pairs_in": 1, "pairs_closed": 1, "pairs_kept": 0}),
        "{v}"
    );
    assert_eq!(v["metadata"]["queued_by_rule"], json!({}));

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
    let adjudication = &v["metadata"]["adjudication"];
    assert_eq!(adjudication["failed"], 1, "{v}");
    assert_eq!(adjudication["pairs_kept"], 1, "{v}");
    assert_eq!(
        v["metadata"]["queued_by_rule"],
        json!({"safety_member_change": 1})
    );
}

// ----- B4: a gate duplicate is a restatement about the SAME subject --------

/// `remember --facts-json` with a novelty gate loose enough that the offline
/// stub's vectors for two people on the same drug read as duplicates.
fn gated(store: &std::path::Path, subject: &str, predicate: &str) -> Value {
    let facts = json!([{ "subject": subject, "predicate": predicate, "object": "metformin" }]);
    run_json(
        engram(store)
            .env("ENGRAM_FACT_DUP_THRESHOLD", "0.30")
            .env("ENGRAM_FACT_NOVEL_THRESHOLD", "0.10"),
        &[
            "remember",
            &format!("{subject} {predicate} metformin."),
            "--kb",
            "personal",
            "--facts-json",
            &facts.to_string(),
        ],
    )
}

/// The report's repro: Carol's metformin was absorbed as a confirmation of
/// Bob's, and Carol had no fact at all.
#[test]
fn a_gate_duplicate_never_crosses_subjects() {
    let tmp = store();
    let bob = gated(tmp.path(), "Bob Smith", "takes_medication");
    assert_eq!(bob["data"]["facts_added"], 1, "{bob}");
    let carol = gated(tmp.path(), "Carol Diaz", "takes_medication");
    assert_eq!(carol["data"]["facts_added"], 1, "{carol}");
    assert_eq!(carol["data"]["facts_deduped"], 0, "{carol}");
    assert_eq!(carol["metadata"]["gate"]["duplicate"], 0, "{carol}");
    for who in ["Bob Smith", "Carol Diaz"] {
        let facts = facts_for(tmp.path(), who);
        assert_eq!(facts.len(), 1, "{who}: {facts:?}");
    }

    // Within one subject the gate still does its job: the same statement
    // under a relation the registry does not know is absorbed.
    let again = gated(tmp.path(), "Bob Smith", "uses");
    assert_eq!(again["metadata"]["gate"]["duplicate"], 1, "{again}");
    assert_eq!(again["data"]["facts_added"], 0, "{again}");
}

// ----- B9: a gate threshold nobody can use is a config error ---------------

#[test]
fn an_unusable_gate_threshold_is_exit_2_before_anything_is_stored() {
    let tmp = store();
    for (name, value) in [
        ("ENGRAM_FACT_DUP_THRESHOLD", "abc"),
        ("ENGRAM_FACT_DUP_THRESHOLD", "1.5"),
        // Leaves no uncertain band under the default novel threshold (0.65).
        ("ENGRAM_FACT_DUP_THRESHOLD", "0.30"),
    ] {
        let out = engram(tmp.path())
            .env(name, value)
            .args([
                "remember",
                "Ana takes metformin 500 mg.",
                "--kb",
                "personal",
                "--facts-json",
                &med("metformin 500 mg"),
                "--json",
            ])
            .assert()
            .code(2)
            .get_output()
            .stderr
            .clone();
        let err: Value = serde_json::from_slice(&out).expect("JSON error");
        assert_eq!(err["error"]["exit_code"], 2, "{name}={value}: {err}");
        assert!(
            err["error"]["message"]
                .as_str()
                .unwrap_or_default()
                .contains("ENGRAM_FACT_"),
            "the message names the variable: {err}"
        );
    }
    let listed = run_json(&mut engram(tmp.path()), &["memory", "list", "--diary", "*"]);
    assert_eq!(
        listed["data"]["memories"].as_array().map(Vec::len),
        Some(0),
        "{listed}"
    );

    engram(tmp.path())
        .env("ENGRAM_FACT_NOVEL_THRESHOLD", "0.99")
        .args(["facts", "backfill", "--kb", "personal", "--json"])
        .assert()
        .code(2);
}

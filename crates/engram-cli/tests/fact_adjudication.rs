//! `--autonomy full` — the JEV conflict rule fails CLOSED.
//!
//! The rule is a `Heuristic` authority. Whatever the judge answers — or fails
//! to answer — the store may end up with MORE open questions than `assisted`
//! would leave, never fewer on a safety relation, and never a close or an
//! absorb on behalf of an observation it threw away. Safety pairs are never
//! sent to the judge at all.
//!
//! Every test drives the judge through `ENGRAM_JUDGE_STUB`, whose rules match
//! substrings of each question's instructions (see `judge::stub`); keys are
//! stripped by the harness, so nothing here can reach a network.

mod wave3_fact_support;
use serde_json::{json, Value};
use wave3_fact_support::*;

/// The four pair questions, by a phrase each one's wording contains.
fn signals(restates: f64, same_slot: f64, contradicts: f64, old_ended: f64) -> String {
    json!({
        "same claim": restates,
        "same attribute": same_slot,
        "contradict each": contradicts,
        "stopped being": old_ended,
    })
    .to_string()
}

/// A move or a correction, as the judge measured them on the pair set.
fn supersede() -> String {
    signals(0.02, 0.97, 0.8, 0.9)
}

/// A second, compatible value.
fn coexist() -> String {
    signals(0.02, 0.9, 0.03, 0.02)
}

/// The same claim, worded differently.
fn restates() -> String {
    signals(0.97, 0.98, 0.05, 0.02)
}

/// A clash, but the text never says the stored value ended.
fn undecided() -> String {
    signals(0.02, 0.95, 0.6, 0.05)
}

/// A judge that answers nothing (no rule matches, no `*`): a provider that
/// dropped every answer.
const NO_ANSWER: &str = r#"{"no question says this": 0.5}"#;

/// `remember --facts-json … --autonomy full` with the judge scripted by `spec`.
/// A TypeSafe key is configured, which is consent to judge; the stub beats
/// every key, so nothing leaves the machine.
fn judged(store: &std::path::Path, content: &str, facts_json: &str, spec: &str) -> Value {
    run_json(
        engram(store)
            .env("ENGRAM_JUDGE_STUB", spec)
            .env("TYPESAFE_API_KEY", "ts-test-not-a-key"),
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

// ----- no verdict is not "coexist" -----------------------------------------

#[test]
fn an_unanswered_judgement_leaves_the_question_queued() {
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
        NO_ANSWER,
    );
    assert_eq!(v["data"]["facts_added"], 1, "{v}");
    assert_eq!(queued(&v).len(), 1, "the question must survive: {v}");
    assert!(
        note_kinds(&v).contains(&"adjudication_failed".to_string()),
        "the envelope must say nobody decided: {v}"
    );
    assert_eq!(v["metadata"]["adjudication"]["failed"], 1, "{v}");
    assert_eq!(open_conflicts(tmp.path())["data"]["total_open"], 1);
    assert_eq!(open_objects(tmp.path()), ["1990-04-12", "1990-04-13"]);
}

/// Signals that do not settle a pair leave it to the caller, and that is not
/// a failure: the judge answered, the rule declined.
#[test]
fn an_undecided_pair_stays_queued_without_a_failure() {
    let tmp = store();
    remember_facts(
        tmp.path(),
        "Ana was born 1990-04-12.",
        &birthday("1990-04-12"),
    );
    for object in ["1990-04-13", "1990-04-14"] {
        let v = judged(
            tmp.path(),
            &format!("Ana was born {object}."),
            &birthday(object),
            &undecided(),
        );
        assert!(!queued(&v).is_empty(), "{object}: {v}");
        assert!(note_kinds(&v).is_empty(), "{object}: {v}");
        assert_eq!(v["metadata"]["adjudication"]["failed"], 0, "{v}");
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
        "Ana was born 1990-04-12.",
        &birthday("1990-04-12"),
    );
    let v = run_json(
        &mut engram(tmp.path()),
        &[
            "remember",
            "Ana was born 1990-04-13.",
            "--kb",
            "personal",
            "--autonomy",
            "full",
            "--facts-json",
            &birthday("1990-04-13"),
        ],
    );
    assert_eq!(queued(&v).len(), 1, "{v}");
    assert_eq!(v["metadata"]["adjudication"]["calls"], 0, "{v}");
    assert!(note_kinds(&v).is_empty(), "{v}");
}

/// An OpenRouter key was set for extraction; it is not consent to send the
/// memory text to TypeSafe. `--autonomy full` with a judge reachable but no
/// TypeSafe key and no `judge.adjudicate` setting used to ask anyway. Now
/// nothing is sent and the pair queues with a reason the agent can read.
#[test]
fn full_autonomy_without_consent_sends_nothing_and_says_why() {
    let tmp = store();
    remember_facts(
        tmp.path(),
        "Ana was born 1990-04-12.",
        &birthday("1990-04-12"),
    );
    let mut cmd = engram(tmp.path());
    // The stub stands in for the judge OpenRouter would reach.
    cmd.env("ENGRAM_JUDGE_STUB", supersede())
        .env("OPENROUTER_API_KEY", "sk-or-test-not-a-key");
    let v = run_json(
        &mut cmd,
        &[
            "remember",
            "Ana was born 1990-04-13.",
            "--kb",
            "personal",
            "--no-extract-facts",
            "--autonomy",
            "full",
            "--facts-json",
            &birthday("1990-04-13"),
        ],
    );
    assert_eq!(queued(&v).len(), 1, "the pair is the agent's: {v}");
    assert_eq!(v["metadata"]["adjudication"]["calls"], 0, "{v}");
    let notes = v["data"]["plan_notes"].as_array().cloned().unwrap();
    assert_eq!(notes.len(), 1, "{v}");
    assert_eq!(notes[0]["kind"], "adjudication_failed", "{v}");
    assert!(
        notes[0]["detail"]
            .as_str()
            .unwrap()
            .contains("judge_not_consented"),
        "{v}"
    );

    // An explicit judge.adjudicate opts in through the same route.
    let mut cmd = engram(tmp.path());
    cmd.env("ENGRAM_JUDGE_STUB", supersede())
        .env("OPENROUTER_API_KEY", "sk-or-test-not-a-key")
        .env("ENGRAM_JUDGE_ADJUDICATE", "on");
    let v = run_json(
        &mut cmd,
        &[
            "remember",
            "Ana was born 1990-04-14.",
            "--kb",
            "personal",
            "--no-extract-facts",
            "--autonomy",
            "full",
            "--facts-json",
            &birthday("1990-04-14"),
        ],
    );
    assert!(
        v["metadata"]["adjudication"]["calls"].as_u64().unwrap() >= 1,
        "{v}"
    );
}

// ----- a restatement discards the WHOLE observation ------------------------

/// A caller's birthday correction plans a close of the extractor's guess; the
/// rule then reads the correction as a restatement. The close must not
/// survive — `close_authority=caller`, `closed_by` naming a fact that was
/// never written.
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
        &restates(),
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

// ----- a safety question is the caller's -----------------------------------

/// A dose change is a question only the caller may settle, so no judge is
/// even asked: whatever it would say, the pair queues, the caller's dose is
/// stored, and nothing about the medication leaves the machine.
#[test]
fn a_safety_pair_is_never_sent_to_the_judge() {
    for spec in [restates(), coexist(), supersede()] {
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
            &spec,
        );
        assert_eq!(
            v["data"]["facts_added"], 1,
            "the caller's dose is kept: {v}"
        );
        assert_eq!(v["data"]["facts_deduped"], 0, "{v}");
        assert_eq!(queued(&v).len(), 1, "{v}");
        assert_eq!(queued(&v)[0]["suggested_action"], "requires_context");
        assert!(note_kinds(&v).is_empty(), "{v}");
        let adjudication = &v["metadata"]["adjudication"];
        assert_eq!(adjudication["calls"], 0, "{v}");
        assert_eq!(adjudication["pairs_in"], 0, "{v}");
        assert_eq!(adjudication["not_asked"], 0, "{v}");
        assert_eq!(open_conflicts(tmp.path())["data"]["total_open"], 1);
        assert_eq!(
            open_objects(tmp.path()),
            ["metformin 1000 mg", "metformin 500 mg"]
        );
    }
}

/// Outside the safety registry the rule's answers stand — `--autonomy full`
/// is not `assisted` under another name.
#[test]
fn the_rule_settles_routine_questions() {
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
        &coexist(),
    );
    assert_eq!(queued(&v).len(), 0, "{v}");
    assert!(note_kinds(&v).is_empty(), "{v}");

    let v = judged(
        tmp.path(),
        "Ana was born 1990-04-14.",
        &birthday("1990-04-14"),
        &restates(),
    );
    assert_eq!(v["data"]["facts_added"], 0, "{v}");
    assert_eq!(v["data"]["facts_deduped"], 1, "{v}");
    assert_eq!(open_objects(tmp.path()), ["1990-04-12", "1990-04-13"]);
}

/// Each pair gets its own ruling: the one the text says ended closes, the
/// one it says nothing about stays a question.
#[test]
fn each_pair_is_ruled_on_its_own() {
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
    // The longest matching key wins, so the stored 04-13 fact's own object
    // overrides the per-question phrases for its four questions: a weak clash
    // (0.15) the rule leaves to the caller. 04-12 gets a clear supersede.
    let mut spec: Value = serde_json::from_str(&supersede()).unwrap();
    spec[r#""object":"1990-04-13""#] = json!(0.15);
    let spec = spec.to_string();
    let v = judged(
        tmp.path(),
        "Ana was born 1990-04-14, not the 12th.",
        &birthday("1990-04-14"),
        &spec,
    );
    assert_eq!(v["data"]["conflicts"].as_array().unwrap().len(), 1, "{v}");
    let asked: Vec<Value> = queued(&v);
    assert_eq!(asked.len(), 1, "the 04-13 pair is still a question: {v}");
    assert_eq!(asked[0]["old"]["object"], "1990-04-13");
    assert_eq!(open_objects(tmp.path()), ["1990-04-13", "1990-04-14"]);
}

/// `judge.adjudicate=shadow`: the judge is asked and its real ruling is
/// journaled with `applied=false`, but nothing is applied — the pair stays
/// queued. Shadow used to replace the answers with a failure, so a shadow run
/// journaled `action=failed` and could never be used to evaluate the rule.
#[test]
fn shadow_journals_the_real_ruling_and_applies_nothing() {
    let tmp = store();
    remember_facts(
        tmp.path(),
        "Ana was born 1990-04-12.",
        &birthday("1990-04-12"),
    );
    let v = run_json(
        engram(tmp.path())
            .env("ENGRAM_JUDGE_STUB", supersede())
            .env("TYPESAFE_API_KEY", "ts-test-not-a-key")
            .env("ENGRAM_JUDGE_ADJUDICATE", "shadow"),
        &[
            "remember",
            "Ana was born 1990-04-13.",
            "--kb",
            "personal",
            "--autonomy",
            "full",
            "--facts-json",
            &birthday("1990-04-13"),
        ],
    );
    let asked = queued(&v);
    assert_eq!(asked.len(), 1, "shadow applied the verdict: {v}");
    let notes = note_kinds(&v);
    assert!(
        !notes.contains(&"adjudication_stale".to_string()),
        "a fresh shadow verdict is not stale: {v}"
    );
    assert_eq!(
        open_objects(tmp.path()),
        ["1990-04-12", "1990-04-13"],
        "shadow closed a fact: {v}"
    );
    let ledger = engram_storage::SqliteStore::open(tmp.path().join("db.sqlite"))
        .expect("the temp store opens");
    let rows = ledger
        .decisions_for_key("personal", asked[0]["decision_key"].as_str().unwrap())
        .expect("history");
    assert_eq!(rows.len(), 1, "{v}");
    let note = rows[0].note.as_deref().unwrap_or_default();
    assert!(
        note.starts_with("action=supersede applied=false model="),
        "{note}"
    );
    assert!(note.contains("contradicts="), "signals journaled: {note}");
}

// ----- judged outside the write lock, applied only to what it saw ----------

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
        &coexist(),
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
    // The unused verdict is still journaled on the pair it judged, with the
    // judge's real answer and a mark that it was not applied.
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
        .starts_with("action=coexist applied=false model="));
}

/// Two facts of one memory that conflict only with EACH OTHER. When the batch
/// is judged the store holds neither, so there is nothing to ask; the pair
/// exists only once the first has landed. It stays queued, and the answer
/// says a judge was never asked — not that a verdict went stale.
#[test]
fn a_conflict_inside_one_batch_says_no_judge_was_asked() {
    let tmp = store();
    let v = judged(
        tmp.path(),
        "Ana was born on the 12th, or the 13th.",
        r#"[{"subject":"Ana","predicate":"birthday","object":"1990-04-12"},
            {"subject":"Ana","predicate":"birthday","object":"1990-04-13"}]"#,
        &coexist(),
    );
    assert_eq!(queued(&v).len(), 1, "{v}");
    assert_eq!(note_kinds(&v), ["adjudication_not_asked"], "{v}");
    let adjudication = &v["metadata"]["adjudication"];
    assert_eq!(adjudication["calls"], 0, "{v}");
    assert_eq!(adjudication["stale"], 0, "{v}");
    assert_eq!(adjudication["not_asked"], 1, "{v}");
}

// ----- which judge answered: journaled, never binding ----------------------

/// A routine question the rule settled. Its answer is journaled — `void`,
/// `machine`, `judge:<provider>`, with the four probabilities — and a later
/// write of the same question (same kb, so the same semantic key, from
/// another diary) is planned exactly as in a store that never met the judge:
/// asked again, nothing replayed.
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
        &restates(),
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
    assert_eq!(row.actor, "judge:stub");
    let note = row.note.as_deref().unwrap_or_default();
    assert_eq!(
        note,
        "action=reject_new model=stub rule=immutable_attribute_disagreement \
         restates=0.97 same_slot=0.98 contradicts=0.05 old_ended=0.02"
    );
}

/// A failure is an answer too, as far as the journal is concerned: the pairs
/// stay queued AND the ledger says which judge failed to decide them.
#[test]
fn a_failed_judgement_is_journaled_as_failed() {
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
        NO_ANSWER,
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

/// The judge's journal row is a `void`: it rules on nothing, so a caller who
/// answers the question it failed to settle has overruled nobody. `reflect`
/// read every `machine` row as a ruling and proposed the lesson "the machine
/// ruled `void` and the caller ruled `keep_both`" — exactly on the questions
/// this lane routes to the caller (a failed or stale judge).
#[test]
fn a_caller_answering_after_the_judge_overrules_nobody() {
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
        NO_ANSWER,
    );
    let id = queued(&v)[0]["conflict_id"]
        .as_str()
        .expect("queued")
        .to_string();
    run_json(
        &mut engram(tmp.path()),
        &["facts", "resolve", &id, "--action", "keep-both"],
    );
    let reflected = run_json(&mut engram(tmp.path()), &["reflect", "--kb", "personal"]);
    assert_eq!(reflected["data"]["candidates"], 0, "{reflected}");
    assert_eq!(reflected["data"]["lessons_written"], 0, "{reflected}");
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

    let v = judged(
        tmp.path(),
        "Ana was born 1990-04-13, not the 12th.",
        &birthday("1990-04-13"),
        &supersede(),
    );
    assert_eq!(v["metadata"]["gate"]["skipped_has_siblings"], 1, "{v}");
    assert_eq!(
        v["metadata"]["adjudication"],
        json!({"calls": 1, "failed": 0, "supersede": 1, "coexist": 0, "reject_new": 0,
               "stale": 0, "not_asked": 0, "pairs_in": 1, "pairs_closed": 1, "pairs_kept": 0}),
        "{v}"
    );
    assert_eq!(v["metadata"]["queued_by_rule"], json!({}));

    remember_facts(
        tmp.path(),
        "Ana was born 1990-04-14.",
        &birthday("1990-04-14"),
    );
    let v = judged(
        tmp.path(),
        "Ana was born 1990-04-15.",
        &birthday("1990-04-15"),
        NO_ANSWER,
    );
    // 04-15 against the open 04-13 and 04-14: both stay asked.
    let adjudication = &v["metadata"]["adjudication"];
    assert_eq!(adjudication["failed"], 1, "{v}");
    assert_eq!(adjudication["pairs_kept"], 2, "{v}");
    assert_eq!(adjudication["pairs_closed"], 0, "{v}");
}

// ----- B4: a gate duplicate is a restatement about the SAME subject --------

/// `remember --facts-json` with a novelty gate loose enough that the offline
/// stub's vectors for two short statements about one drug read as duplicates.
fn gated(store: &std::path::Path, subject: &str, predicate: &str, object: &str) -> Value {
    let facts = json!([{ "subject": subject, "predicate": predicate, "object": object }]);
    run_json(
        engram(store)
            .env("ENGRAM_FACT_DUP_THRESHOLD", "0.30")
            .env("ENGRAM_FACT_NOVEL_THRESHOLD", "0.10"),
        &[
            "remember",
            &format!("{subject} {predicate} {object}."),
            "--kb",
            "personal",
            "--facts-json",
            &facts.to_string(),
        ],
    )
}

fn predicates_of(store: &std::path::Path, subject: &str) -> Vec<String> {
    let mut predicates: Vec<String> = facts_for(store, subject)
        .iter()
        .filter_map(|f| f["predicate"].as_str().map(str::to_string))
        .collect();
    predicates.sort();
    predicates
}

/// The report's repro: Carol's metformin was absorbed as a confirmation of
/// Bob's, and Carol had no fact at all.
#[test]
fn a_gate_duplicate_never_crosses_subjects() {
    let tmp = store();
    let bob = gated(tmp.path(), "Bob Smith", "takes_medication", "metformin");
    assert_eq!(bob["data"]["facts_added"], 1, "{bob}");
    let carol = gated(tmp.path(), "Carol Diaz", "takes_medication", "metformin");
    assert_eq!(carol["data"]["facts_added"], 1, "{carol}");
    assert_eq!(carol["data"]["facts_deduped"], 0, "{carol}");
    assert_eq!(carol["metadata"]["gate"]["duplicate"], 0, "{carol}");
    for who in ["Bob Smith", "Carol Diaz"] {
        let facts = facts_for(tmp.path(), who);
        assert_eq!(facts.len(), 1, "{who}: {facts:?}");
    }

    // Within one subject the gate still does its job: the same statement
    // under a relation the registry does not know is absorbed.
    gated(tmp.path(), "Dan Reyes", "likes", "chess");
    let again = gated(tmp.path(), "Dan Reyes", "enjoys", "chess");
    assert_eq!(again["metadata"]["gate"]["duplicate"], 1, "{again}");
    assert_eq!(again["data"]["facts_added"], 0, "{again}");
    assert_eq!(predicates_of(tmp.path(), "Dan Reyes"), ["likes"]);
}

/// A gate duplicate is an absorb across relations, and an absorb is a
/// heuristic discarding the caller's observation: Bob's penicillin ALLERGY
/// vanished as a restatement of Bob taking penicillin, with no question and
/// no note. On a safety relation, either side, the classifier decides.
#[test]
fn a_gate_duplicate_never_discards_a_safety_fact() {
    let tmp = store();
    gated(tmp.path(), "Bob Smith", "takes_medication", "penicillin");
    let allergy = gated(tmp.path(), "Bob Smith", "allergic_to", "penicillin");
    assert_eq!(allergy["data"]["facts_added"], 1, "{allergy}");
    assert_eq!(allergy["data"]["facts_deduped"], 0, "{allergy}");
    assert_eq!(allergy["metadata"]["gate"]["duplicate"], 0, "{allergy}");
    assert_eq!(allergy["metadata"]["gate"]["uncertain"], 1, "{allergy}");

    // A routine statement is not absorbed into a safety fact either: that
    // would count as a confirmation of the medication.
    let uses = gated(tmp.path(), "Bob Smith", "uses", "penicillin");
    assert_eq!(uses["data"]["facts_added"], 1, "{uses}");
    assert_eq!(uses["metadata"]["gate"]["duplicate"], 0, "{uses}");
    assert_eq!(
        predicates_of(tmp.path(), "Bob Smith"),
        ["allergic_to", "takes_medication", "uses"]
    );
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

    // An extraction LLM resolves (the stub), so exit 2 here can only be the
    // threshold: without one, backfill exits 2 on the missing key anyway.
    let out = engram(tmp.path())
        .env("ENGRAM_FACT_LLM_STUB", r#"{"*":"[]"}"#)
        .env("ENGRAM_FACT_NOVEL_THRESHOLD", "0.99")
        .args(["facts", "backfill", "--kb", "personal", "--json"])
        .assert()
        .code(2)
        .get_output()
        .stderr
        .clone();
    let err: Value = serde_json::from_slice(&out).expect("JSON error");
    assert!(
        err["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("ENGRAM_FACT_NOVEL_THRESHOLD"),
        "{err}"
    );
}

// ----- B16: the dedup answer carries every always-present field ------------

#[test]
fn a_duplicate_remember_answers_with_every_required_field() {
    let tmp = store();
    let schema = run_json(
        &mut engram(tmp.path()),
        &["agent-info", "--schema", "remember"],
    );
    let required: Vec<String> = schema["properties"]["output"]["properties"]["data"]["required"]
        .as_array()
        .expect("remember publishes required data fields")
        .iter()
        .filter_map(|f| f.as_str().map(str::to_string))
        .collect();
    assert!(
        required.contains(&"fact_extraction".to_string()),
        "{required:?}"
    );

    let content = "Ana takes metformin 500 mg.";
    let remember = |extra: &[&str]| {
        let mut args = vec!["remember", content, "--kb", "personal"];
        args.extend_from_slice(extra);
        run_json(&mut engram(tmp.path()), &args)
    };
    let first = remember(&["--no-extract-facts"]);
    assert_eq!(first["data"]["stored"], true, "{first}");

    let facts = med("metformin 500 mg");
    for (extra, status) in [
        (vec!["--no-extract-facts"], "disabled"),
        (vec!["--facts-json", facts.as_str()], "skipped_facts_json"),
        (vec![], "skipped_duplicate"),
    ] {
        let v = remember(&extra);
        assert_eq!(v["data"]["deduplicated"], true, "{v}");
        for field in &required {
            assert!(
                v["data"].get(field).is_some(),
                "{extra:?}: `{field}` is always-present in the schema: {v}"
            );
        }
        assert_eq!(v["data"]["chunks"], 0, "nothing re-embedded: {v}");
        assert_eq!(v["data"]["fact_extraction"], status, "{extra:?}: {v}");
        assert_eq!(v["metadata"]["fact_extraction"], status, "{v}");
    }
}

// ----- the polypharmacy guarantee holds under --autonomy full --------------

const MEDS: [&str; 5] = [
    "empagliflozin 12.5 mg daily",
    "rapamycin 6 mg weekly",
    "metformin 500 mg twice daily",
    "lisinopril 10 mg daily",
    "levothyroxine 75 mcg daily",
];

/// A judge that would retire every fact it is shown, if it were ever asked.
fn supersede_everything(_store: &std::path::Path) -> String {
    supersede()
}

fn assert_five_open_and_untouched(store: &std::path::Path) {
    assert_eq!(open_conflicts(store)["data"]["total_open"], 0);
    let facts = facts_for(store, "Ana");
    assert_eq!(facts.len(), 5, "{facts:?}");
    for f in &facts {
        assert!(f["valid_until"].is_null(), "an interval was closed: {f}");
        assert!(f["superseded_by"].is_null(), "{f}");
    }
}

/// Five medications are five members of a set, never a question, so the
/// judge is never consulted — not even one that answers `supersede` for
/// everything it is shown.
#[test]
fn five_medications_under_full_autonomy_close_nothing() {
    let tmp = store();
    for m in MEDS {
        let reply = supersede_everything(tmp.path());
        let v = judged(tmp.path(), &format!("Ana takes {m}."), &med(m), &reply);
        assert_eq!(v["data"]["facts_added"], 1, "{m}: {v}");
        assert_eq!(queued(&v).len(), 0, "{m} queued a question: {v}");
        assert_eq!(
            v["data"]["conflicts"].as_array().map(Vec::len),
            Some(0),
            "{m} closed something: {v}"
        );
        assert_eq!(v["metadata"]["adjudication"]["calls"], 0, "{m}: {v}");
    }
    assert_five_open_and_untouched(tmp.path());
}

#[test]
fn five_medications_from_one_memory_under_full_autonomy_close_nothing() {
    let tmp = store();
    let triples: Vec<Value> = MEDS
        .iter()
        .map(|m| json!({"subject": "Ana", "predicate": "takes_medication", "object": m}))
        .collect();
    let v = judged(
        tmp.path(),
        "Ana's current regimen: empagliflozin, rapamycin, metformin, lisinopril, levothyroxine.",
        &Value::Array(triples).to_string(),
        &supersede_everything(tmp.path()),
    );
    assert_eq!(v["data"]["facts_added"], 5, "{v}");
    assert_eq!(queued(&v).len(), 0, "{v}");
    assert_eq!(v["data"]["conflicts"].as_array().map(Vec::len), Some(0));
    assert_eq!(v["metadata"]["adjudication"]["calls"], 0, "{v}");
    assert_five_open_and_untouched(tmp.path());
}

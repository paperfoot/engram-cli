//! `compile --llm` grounding: what the lexical rules and the judge let into
//! the canonical fact store.
//!
//! Every run is offline: the extractor is `ENGRAM_COMPILER_LLM_STUB`, the
//! judge (when asked at all) is `ENGRAM_JUDGE_STUB`, and the shared harness
//! strips every provider key.

mod wave3_fact_support;

use serde_json::Value;
use std::path::Path;
use wave3_fact_support::{engram, run_json, store};

const KB: &str = "corpus";

fn seed(store: &Path, content: &str) {
    run_json(
        &mut engram(store),
        &["remember", content, "--kb", KB, "--no-extract-facts"],
    );
}

/// An extractor script: `(chunk substring, evidence object)` rules, and no
/// evidence for any other chunk.
fn extractor(rules: &[(&str, &str)]) -> String {
    let rules: Vec<String> = rules
        .iter()
        .map(|(key, evidence)| format!("{key:?}: {evidence}"))
        .collect();
    format!(r#"{{{}, "*": {{}}}}"#, rules.join(", "))
}

fn fact(subject: &str, predicate: &str, object: &str) -> String {
    format!(
        r#"{{"facts":[{{"subject":"{subject}","predicate":"{predicate}","object":"{object}","confidence":0.9}}]}}"#
    )
}

/// `compile --llm` with `env` on top of the harness; the compile report.
fn compile(store: &Path, script: &str, env: &[(&str, &str)]) -> Value {
    let mut cmd = engram(store);
    cmd.env("ENGRAM_COMPILER_LLM_STUB", script)
        .env_remove("ENGRAM_JUDGE_LOG");
    for (key, value) in env {
        cmd.env(key, value);
    }
    run_json(&mut cmd, &["compile", "--kb", KB, "--all", "--llm"])["data"]["compile"].clone()
}

fn facts_about(store: &Path, subject: &str) -> Vec<Value> {
    run_json(
        &mut engram(store),
        &["facts", "list", "--subject", subject, "--kb", KB],
    )["data"]["facts"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// Confirmed on 0.4.1: "Bob" and "HIV" are under four characters, so the old
/// word-overlap rule had nothing to check and admitted the fact against any
/// chunk, and `state Bob` served an open diagnosis from a weather note.
#[test]
fn a_fact_about_someone_the_chunk_never_names_is_rejected_with_the_judge_off() {
    let s = store();
    seed(
        s.path(),
        "The weather in Lisbon was sunny all week and the ferries ran on time.",
    );
    let script = extractor(&[("weather in Lisbon", &fact("Bob", "diagnosed_with", "HIV"))]);
    let stats = compile(s.path(), &script, &[]);
    assert_eq!(stats["llm_facts"], 0, "{stats}");
    assert_eq!(stats["llm_facts_rejected"], 1, "{stats}");
    assert!(facts_about(s.path(), "Bob").is_empty());
}

/// "Sam takes no blood thinners" put into the triple shape lists "no blood
/// thinners" as a medication. The text does say it, so no grounding rule
/// can object: the shape is checked for the safety sets in every mode.
#[test]
fn a_negated_safety_member_is_rejected_and_a_grounded_one_is_written() {
    let s = store();
    seed(
        s.path(),
        "Sam takes no blood thinners before the knee surgery next week.",
    );
    seed(
        s.path(),
        "Ana takes metformin 500 mg daily for her diabetes.",
    );
    let script = extractor(&[
        (
            "no blood thinners",
            &fact("Sam", "takes_medication", "no blood thinners"),
        ),
        (
            "metformin 500 mg",
            &fact("Ana", "takes_medication", "metformin 500 mg"),
        ),
    ]);
    let stats = compile(s.path(), &script, &[]);
    assert_eq!(stats["llm_facts"], 1, "{stats}");
    assert_eq!(stats["llm_facts_rejected"], 1, "{stats}");
    assert!(facts_about(s.path(), "Sam").is_empty());
    let ana = facts_about(s.path(), "Ana");
    assert_eq!(ana.len(), 1, "{ana:?}");
    assert_eq!(ana[0]["object"], "metformin 500 mg");
}

// ---------------------------------------------------------------------------
// The grounding judge (`judge.grounding`), scripted with ENGRAM_JUDGE_STUB.
// ---------------------------------------------------------------------------

/// Two memories: a trial result the extractor inverts (lexically grounded,
/// wrong) next to a true claim, and a medication the extractor reads once
/// right and once with an invented dose.
fn seed_corpus(store: &Path) {
    seed(
        store,
        "In this cohort, rapamycin did not decrease mortality compared with placebo, \
         but it lowered fasting insulin in every participant.",
    );
    seed(store, "Ana takes metformin 500 mg daily for her diabetes.");
}

fn corpus_extractor() -> String {
    extractor(&[
        (
            "did not decrease mortality",
            r#"{"claims":[
                {"content":"Rapamycin decreased mortality compared with placebo."},
                {"content":"Rapamycin lowered fasting insulin in every participant."}]}"#,
        ),
        (
            "metformin 500 mg daily",
            r#"{"facts":[
                {"subject":"Ana","predicate":"takes_medication","object":"metformin 500 mg"},
                {"subject":"Ana","predicate":"takes_medication","object":"metformin 850 mg"}]}"#,
        ),
    ])
}

/// The inverted claim scores 0.1; everything else, the invented dose
/// included, scores 0.95.
const JUDGE: &str = r#"{"decreased mortality": 0.1, "*": 0.95}"#;

fn medications(store: &Path) -> Vec<Value> {
    facts_about(store, "Ana")
        .iter()
        .map(|f| f["object"].clone())
        .collect()
}

#[test]
fn on_the_judge_drops_the_inverted_claim_and_the_numbers_stay_in_code() {
    let s = store();
    seed_corpus(s.path());
    let stats = compile(
        s.path(),
        &corpus_extractor(),
        &[
            ("ENGRAM_JUDGE_GROUNDING", "on"),
            ("ENGRAM_JUDGE_STUB", JUDGE),
        ],
    );
    assert_eq!(stats["judge_mode"], "on", "{stats}");
    assert_eq!(stats["judge_route"], "stub", "{stats}");
    assert_eq!(stats["judge_requests"], 2, "{stats}");
    assert_eq!(stats["judge_fallback_chunks"], 0, "{stats}");
    // The lexical rule admits "decreased mortality" (5 of its 6 words are in
    // the chunk); the judge does not.
    assert_eq!(stats["llm_claims"], 1, "{stats}");
    assert_eq!(stats["llm_claims_rejected"], 1, "{stats}");
    assert_eq!(stats["judge_claims_admitted_by_both"], 1, "{stats}");
    assert_eq!(stats["judge_claims_lexical_only_admitted"], 1, "{stats}");
    // 850 mg scored 0.95 and is still dropped: it is not in the chunk.
    assert_eq!(stats["llm_facts"], 1, "{stats}");
    assert_eq!(stats["llm_facts_rejected"], 1, "{stats}");
    assert_eq!(stats["judge_facts_admitted_by_both"], 1, "{stats}");
    assert_eq!(stats["judge_facts_rejected_by_both"], 1, "{stats}");
    assert_eq!(medications(s.path()), vec!["metformin 500 mg"]);
}

#[test]
fn shadow_stores_what_off_stores_and_counts_what_on_would_change() {
    let off = store();
    seed_corpus(off.path());
    let off_stats = compile(off.path(), &corpus_extractor(), &[]);
    let shadow = store();
    seed_corpus(shadow.path());
    let shadow_stats = compile(
        shadow.path(),
        &corpus_extractor(),
        &[
            ("ENGRAM_JUDGE_GROUNDING", "shadow"),
            ("ENGRAM_JUDGE_STUB", JUDGE),
        ],
    );
    for key in [
        "llm_claims",
        "llm_claims_rejected",
        "llm_facts",
        "llm_facts_rejected",
    ] {
        assert_eq!(off_stats[key], shadow_stats[key], "{key}: {shadow_stats}");
    }
    assert_eq!(off_stats["llm_claims"], 2, "{off_stats}");
    assert_eq!(medications(off.path()), medications(shadow.path()));

    assert_eq!(off_stats["judge_mode"], "off", "{off_stats}");
    assert!(off_stats["judge_route"].is_null(), "{off_stats}");
    assert_eq!(off_stats["judge_claims_lexical_only_admitted"], 0);
    assert_eq!(shadow_stats["judge_mode"], "shadow", "{shadow_stats}");
    assert_eq!(shadow_stats["judge_claims_admitted_by_both"], 1);
    assert_eq!(shadow_stats["judge_claims_lexical_only_admitted"], 1);
    assert_eq!(shadow_stats["judge_facts_admitted_by_both"], 1);
    assert_eq!(shadow_stats["judge_facts_rejected_by_both"], 1);
}

/// The judge answers the inverted claim but not its neighbour, so it did not
/// decide that chunk: the lexical rules do, and the 0.1 is not used. The
/// medication chunk it answered in full is still judged.
#[test]
fn a_chunk_the_judge_leaves_half_answered_falls_back_to_the_lexical_rules() {
    let s = store();
    seed_corpus(s.path());
    let stats = compile(
        s.path(),
        &corpus_extractor(),
        &[
            ("ENGRAM_JUDGE_GROUNDING", "on"),
            (
                "ENGRAM_JUDGE_STUB",
                r#"{"decreased mortality": 0.1, "metformin": 0.95}"#,
            ),
        ],
    );
    assert_eq!(stats["judge_fallback_chunks"], 1, "{stats}");
    assert_eq!(stats["llm_claims"], 2, "{stats}");
    assert_eq!(stats["llm_claims_rejected"], 0, "{stats}");
    assert_eq!(stats["judge_claims_lexical_only_admitted"], 0, "{stats}");
    assert_eq!(stats["judge_facts_admitted_by_both"], 1, "{stats}");
    assert_eq!(medications(s.path()), vec!["metformin 500 mg"]);
}

/// A judge request carries a memory to a third party: `off` must not even
/// look at the judge's settings, and a site set to shadow with no reachable
/// judge runs as off and says so.
#[test]
fn off_never_reads_the_judge_settings_and_no_route_runs_as_off() {
    let s = store();
    seed_corpus(s.path());
    let stats = compile(
        s.path(),
        &corpus_extractor(),
        &[("ENGRAM_JUDGE_STUB", "not a judge spec")],
    );
    assert_eq!(stats["judge_mode"], "off", "{stats}");
    assert_eq!(stats["llm_claims"], 2, "{stats}");

    // The same stub is read, and refused, once the site asks the judge.
    let out = engram(s.path())
        .env("ENGRAM_COMPILER_LLM_STUB", corpus_extractor())
        .env("ENGRAM_JUDGE_GROUNDING", "shadow")
        .env("ENGRAM_JUDGE_STUB", "not a judge spec")
        .args(["compile", "--kb", KB, "--all", "--llm", "--json"])
        .output()
        .expect("engram runs");
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("ENGRAM_JUDGE_STUB"),
        "{out:?}"
    );

    // No stub and no key (the harness strips them and forces stub
    // embeddings, which also rules the judge out).
    let stats = compile(
        s.path(),
        &corpus_extractor(),
        &[("ENGRAM_JUDGE_GROUNDING", "shadow")],
    );
    assert_eq!(stats["judge_mode"], "shadow", "{stats}");
    assert_eq!(stats["judge_route"], "none", "{stats}");
    assert_eq!(stats["judge_requests"], 0, "{stats}");
    assert_eq!(stats["judge_claims_admitted_by_both"], 0, "{stats}");
    assert_eq!(stats["llm_claims"], 2, "{stats}");
}

/// `ENGRAM_JUDGE_LOG` gets one line per judged item: the calibration record
/// for a user's own corpus, never part of the command's output.
#[test]
fn the_judge_log_records_every_judged_item() {
    let s = store();
    seed_corpus(s.path());
    let log = s.path().join("judge.jsonl");
    compile(
        s.path(),
        &corpus_extractor(),
        &[
            ("ENGRAM_JUDGE_GROUNDING", "shadow"),
            ("ENGRAM_JUDGE_STUB", JUDGE),
            ("ENGRAM_JUDGE_LOG", log.to_str().unwrap()),
        ],
    );
    let lines: Vec<Value> = std::fs::read_to_string(&log)
        .expect("log written")
        .lines()
        .map(|l| serde_json::from_str(l).expect("one JSON object per line"))
        .collect();
    assert_eq!(lines.len(), 4, "{lines:?}");
    let inverted = lines
        .iter()
        .find(|l| l["text"] == "Rapamycin decreased mortality compared with placebo.")
        .expect("the inverted claim is logged");
    assert_eq!(inverted["site"], "compile");
    assert_eq!(inverted["kind"], "claim");
    assert_eq!(inverted["mode"], "shadow");
    assert_eq!(inverted["lexical"], true);
    // Shadow: what was stored, which is the lexical decision.
    assert_eq!(inverted["admitted"], true);
    assert!(inverted["p"].as_f64().unwrap() < 0.2, "{inverted}");
    assert_eq!(lines.iter().filter(|l| l["kind"] == "fact").count(), 2);
}

// ---------------------------------------------------------------------------
// A cessation ends an open safety member, so the lexical rule never admits one.
// ---------------------------------------------------------------------------

/// A true medication compiled from its note, then a later note that only
/// mentions the drug, which the extractor reads as `stopped_taking`. Two
/// compiles, so no chunk-order race decides which fact is written first.
fn seed_cessation(store: &Path) -> String {
    seed(store, "Ana takes metformin 500 mg daily for her diabetes.");
    let stats = compile(
        store,
        &extractor(&[(
            "for her diabetes",
            &fact("Ana", "takes_medication", "metformin"),
        )]),
        &[],
    );
    assert_eq!(stats["llm_facts"], 1, "{stats}");
    seed(
        store,
        "At lunch Ana talked about metformin prices at the pharmacy.",
    );
    extractor(&[(
        "metformin prices",
        &fact("Ana", "stopped_taking", "metformin"),
    )])
}

/// Found in review: "stopped_taking" is not a safety relation by name, and
/// the subject+object rule cannot see a predicate, so this note's cessation
/// reached the writer, which closed the true medication when both facts came
/// from one compile (`state Ana`: no_open_values).
#[test]
fn off_a_safety_cessation_is_refused() {
    let s = store();
    let script = seed_cessation(s.path());
    let stats = compile(s.path(), &script, &[]);
    assert_eq!(stats["llm_facts"], 0, "{stats}");
    assert_eq!(stats["llm_facts_queued"], 0, "{stats}");
    assert_eq!(stats["llm_facts_rejected"], 1, "{stats}");
    assert_eq!(medications(s.path()), vec!["metformin"]);
}

/// With the judge on, the judge decides a cessation (refused at 0.1, passed
/// to the writer at 0.95); shadow keeps off's refusal and counts the
/// disagreement.
#[test]
fn on_the_judge_decides_a_safety_cessation() {
    let judged = |mode: &str, judge: &str| {
        let s = store();
        let script = seed_cessation(s.path());
        compile(
            s.path(),
            &script,
            &[
                ("ENGRAM_JUDGE_GROUNDING", mode),
                ("ENGRAM_JUDGE_STUB", judge),
            ],
        )
    };
    let refused = judged("on", r#"{"stopped": 0.1, "*": 0.95}"#);
    assert_eq!(refused["llm_facts_rejected"], 1, "{refused}");
    assert_eq!(refused["judge_facts_rejected_by_both"], 1, "{refused}");

    let admitted = judged("on", r#"{"*": 0.95}"#);
    assert_eq!(admitted["llm_facts_rejected"], 0, "{admitted}");
    assert_eq!(admitted["judge_facts_judge_only_admitted"], 1, "{admitted}");

    let shadow = judged("shadow", r#"{"*": 0.95}"#);
    assert_eq!(shadow["llm_facts_rejected"], 1, "{shadow}");
    assert_eq!(shadow["judge_facts_judge_only_admitted"], 1, "{shadow}");
}

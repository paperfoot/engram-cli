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

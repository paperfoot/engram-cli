//! Wave 6b — an extraction model's `ended` token is a HINT, not an authority.
//!
//! `CloseAuthority::ExplicitEnd` means the source itself said the state ended,
//! and the write path handed that standing to whatever the extractor put in
//! the `state` field. A model that hallucinated a cessation over text saying
//! the opposite therefore retired a prescription, and stamped the row
//! `close_authority: explicit_end` while doing it [G1].
//!
//! Isolation is `ENGRAM_STORE` on a TempDir and nothing else: no HOME
//! rewriting, so a leaked path cannot reach the developer's real store.

use assert_cmd::prelude::*;
use serde_json::Value;
use std::process::Command;
use tempfile::TempDir;

/// The hallucination, exactly as the gate reproduced it: the memory says she
/// is STILL taking it, the model says it ended.
const HALLUCINATED_END: &str = r#"{"*":[{"subject":"Ana","predicate":"takes_medication",
  "object":"metformin","state":"ended"}]}"#;

fn engram(store: &std::path::Path) -> Command {
    let mut cmd = Command::cargo_bin("engram").expect("binary built");
    cmd.env("ENGRAM_STORE", store);
    cmd.env("ENGRAM_BENCH_FORCE_STUB", "1");
    for k in [
        "GEMINI_API_KEY",
        "COHERE_API_KEY",
        "OPENROUTER_API_KEY",
        "ENGRAM_FACT_LLM_STUB",
        "ENGRAM_FACT_WRITE_FAIL",
        "ENGRAM_FACT_QUEUE_ALL",
    ] {
        cmd.env_remove(k);
    }
    cmd
}

fn run_json(cmd: &mut Command, args: &[&str]) -> Value {
    let out = cmd
        .args(args)
        .arg("--json")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    serde_json::from_slice(&out).expect("valid JSON on stdout")
}

/// A caller's own triples: no LLM, `source_kind = user_stated`.
fn remember_facts(store: &std::path::Path, content: &str, facts_json: &str) -> Value {
    run_json(
        &mut engram(store),
        &[
            "remember",
            content,
            "--kb",
            "personal",
            "--offline",
            "--facts-json",
            facts_json,
        ],
    )
}

/// The same command with an extraction model behind it, scripted offline.
fn remember_extracted(store: &std::path::Path, content: &str, script: &str) -> Value {
    run_json(
        engram(store).env("ENGRAM_FACT_LLM_STUB", script),
        &["remember", content, "--kb", "personal", "--offline"],
    )
}

fn facts_for(store: &std::path::Path, subject: &str) -> Vec<Value> {
    run_json(
        &mut engram(store),
        &["facts", "list", "--subject", subject, "--kb", "personal"],
    )["data"]["facts"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// Active AND historic, so a closed interval is still visible.
fn all_facts(store: &std::path::Path, subject: &str) -> Vec<Value> {
    let shown = run_json(
        &mut engram(store),
        &["facts", "show", subject, "--kb", "personal", "--diary", "*"],
    );
    let mut rows = shown["data"]["active"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    rows.extend(
        shown["data"]["historic"]
            .as_array()
            .cloned()
            .unwrap_or_default(),
    );
    rows
}

/// The audit tag on every row this subject has, in whatever bucket.
fn resolutions(store: &std::path::Path, subject: &str) -> Vec<String> {
    all_facts(store, subject)
        .iter()
        .filter_map(|f| f["resolution"].as_str().map(str::to_string))
        .collect()
}

/// The audit tag on the rows whose interval has ENDED.
fn closed_resolutions(store: &std::path::Path, subject: &str) -> Vec<String> {
    all_facts(store, subject)
        .iter()
        .filter(|f| !f["valid_until"].is_null())
        .filter_map(|f| f["resolution"].as_str().map(str::to_string))
        .collect()
}

fn open_rows(store: &std::path::Path, subject: &str, object: &str) -> Vec<Value> {
    facts_for(store, subject)
        .into_iter()
        .filter(|f| f["object"].as_str() == Some(object) && f["valid_until"].is_null())
        .collect()
}

/// THE BLOCKER. Seed a prescription the caller asserted, then let the model
/// hallucinate a cessation over text that says the opposite.
#[test]
fn a_hallucinated_ending_cannot_close_a_prescription() {
    let tmp = TempDir::new().unwrap();
    remember_facts(
        tmp.path(),
        "Ana is taking metformin.",
        r#"[{"subject":"Ana","predicate":"takes_medication","object":"metformin",
             "state":"ongoing"}]"#,
    );
    let v = remember_extracted(
        tmp.path(),
        "Ana confirms that she is still taking metformin.",
        HALLUCINATED_END,
    );

    assert_eq!(
        open_rows(tmp.path(), "Ana", "metformin").len(),
        1,
        "the prescription is still running: {v}"
    );
    let queued = v["data"]["adjudication_needed"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        queued.len(),
        1,
        "the refused close becomes a question, not a silent skip: {v}"
    );
    assert_eq!(queued[0]["rule"], "extractor_cessation", "{v}");
    assert_eq!(
        queued[0]["suggested_action"], "requires_context",
        "only the person can settle whether the drug stopped: {v}"
    );
    // The claim itself is still recorded — as a point interval, which asserts
    // nothing ongoing — and it may not stamp itself with the caller's
    // authority.
    let stamped = resolutions(tmp.path(), "Ana");
    assert!(
        !stamped.iter().any(|r| r == "explicit_end"),
        "no row may claim the TEXT ended it when only the model did: {stamped:?}"
    );
}

/// The other half of the ruling: a person's own triple keeps the standing it
/// always had, on the same relation and the same store.
#[test]
fn a_caller_stated_ending_still_closes_the_prescription() {
    let tmp = TempDir::new().unwrap();
    remember_facts(
        tmp.path(),
        "Ana started metformin in January 2024.",
        r#"[{"subject":"Ana","predicate":"takes_medication","object":"metformin",
             "valid_from":"2024-01-01"}]"#,
    );
    let v = remember_facts(
        tmp.path(),
        "Ana stopped taking metformin in June 2026.",
        r#"[{"subject":"Ana","predicate":"takes_medication","object":"metformin",
             "state":"ended","valid_until":"2026-06-01"}]"#,
    );
    assert_eq!(
        v["data"]["adjudication_needed"].as_array().unwrap().len(),
        0,
        "a caller's ending is not a question: {v}"
    );
    assert!(
        open_rows(tmp.path(), "Ana", "metformin").is_empty(),
        "a stopped medication must not read as current"
    );
    assert_eq!(
        closed_resolutions(tmp.path(), "Ana"),
        vec!["explicit_end"],
        "the audit tag says the CALLER ended it"
    );
}

/// And an extracted ending still works everywhere the gate allows it —
/// otherwise the fix would have broken the interval model's ordinary case.
#[test]
fn an_extracted_ending_still_closes_a_non_safety_interval() {
    let tmp = TempDir::new().unwrap();
    remember_facts(
        tmp.path(),
        "Bruno lives in Lisbon.",
        r#"[{"subject":"Bruno","predicate":"lives_in","object":"Lisbon",
             "valid_from":"2020-01-01"}]"#,
    );
    let v = remember_extracted(
        tmp.path(),
        "Bruno moved out of Lisbon in June 2026.",
        r#"{"*":[{"subject":"Bruno","predicate":"lives_in","object":"Lisbon",
          "state":"ended","valid_until":"2026-06-01"}]}"#,
    );
    assert!(
        open_rows(tmp.path(), "Bruno", "Lisbon").is_empty(),
        "a non-safety relation is still ended by the text that reports it: {v}"
    );
    assert_eq!(
        closed_resolutions(tmp.path(), "Bruno"),
        vec!["extractor_cessation"],
        "allowed, and recorded for what it is"
    );
}

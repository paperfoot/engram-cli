//! `recall` abstains when nothing stored helps answer the query.
//!
//! Fused and reranked scores are ranks: a query about something the store
//! never heard of used to come back as `verdict: results` with its nearest
//! neighbours. The relevance judge (`judge.relevance`, default on) scores
//! each returned passage in one request; under `on` the ones below the floor
//! are dropped and an all-dropped recall is `no_match` with reason
//! `no_candidate_above_cutoff`. A judge that gives no answer changes nothing.
//!
//! Offline: the judge is `ENGRAM_JUDGE_STUB` (rules keyed by substrings of
//! the passage), and the harness strips every key.

mod wave3_fact_support;

use serde_json::Value;
use std::path::Path;
use wave3_fact_support::*;

const NOEMI: &str = "Noemi Varkoni moved to Ghent in March and works at Halden Labs.";
const ELIAS: &str = "Elias keeps his climbing gear in the blue van.";

fn seeded() -> tempfile::TempDir {
    let tmp = store();
    for content in [NOEMI, ELIAS] {
        run_json(
            &mut engram(tmp.path()),
            &["remember", content, "--kb", "personal", "--no-extract-facts"],
        );
    }
    tmp
}

/// `recall` with a scripted judge; `mode` `None` leaves the site at its
/// default.
fn recall(store: &Path, query: &str, mode: Option<&str>, judge: Option<&str>) -> Value {
    let mut cmd = engram(store);
    if let Some(m) = mode {
        cmd.env("ENGRAM_JUDGE_RELEVANCE", m);
    }
    if let Some(spec) = judge {
        cmd.env("ENGRAM_JUDGE_STUB", spec);
    }
    run_json(&mut cmd, &["recall", query])
}

fn contents(v: &Value) -> Vec<String> {
    v["data"]["results"]
        .as_array()
        .expect("results array")
        .iter()
        .map(|r| r["content"].as_str().unwrap_or_default().to_string())
        .collect()
}

#[test]
fn on_by_default_an_unknown_subject_is_no_match_below_the_cutoff() {
    let tmp = seeded();
    let v = recall(tmp.path(), "What is Rosalie's blood type?", None, Some("0.01"));
    assert_eq!(v["data"]["verdict"], "no_match", "{v}");
    assert_eq!(v["data"]["status"], "no_results", "{v}");
    assert!(contents(&v).is_empty(), "{v}");
    let absence = &v["data"]["absence"];
    assert_eq!(absence["reason"], "no_candidate_above_cutoff", "{v}");
    assert_eq!(absence["threshold"].as_f64(), Some(0.15), "{v}");
    assert_eq!(absence["top_score"].as_f64(), Some(0.01), "{v}");
    assert!(absence["searched_chunks"].as_u64().unwrap() > 0, "{v}");
    let meta = &v["metadata"]["relevance"];
    assert_eq!(meta["mode"], "on", "{v}");
    assert_eq!(meta["abstained"], true, "{v}");
    assert!(meta["dropped"].as_u64().unwrap() > 0, "{v}");
}

#[test]
fn on_keeps_what_helps_answer_and_drops_the_rest() {
    let tmp = seeded();
    let v = recall(
        tmp.path(),
        "Where does Noemi Varkoni work?",
        Some("on"),
        Some(r#"{"Halden Labs": 0.97, "*": 0.02}"#),
    );
    assert_eq!(v["data"]["verdict"], "results", "{v}");
    assert_eq!(contents(&v), vec![NOEMI.to_string()], "{v}");
    assert_eq!(v["data"]["results"][0]["relevance"].as_f64(), Some(0.97));
    assert_eq!(v["metadata"]["relevance"]["dropped"], 1, "{v}");
    assert_eq!(v["metadata"]["relevance"]["abstained"], false, "{v}");
}

#[test]
fn shadow_scores_every_result_and_drops_nothing() {
    let tmp = seeded();
    let v = recall(
        tmp.path(),
        "Where does Noemi Varkoni work?",
        Some("shadow"),
        Some(r#"{"Halden Labs": 0.97, "*": 0.02}"#),
    );
    assert_eq!(contents(&v).len(), 2, "{v}");
    assert!(v["data"]["results"]
        .as_array()
        .unwrap()
        .iter()
        .all(|r| r["relevance"].is_number()));
    assert_eq!(v["metadata"]["relevance"]["dropped"], 0, "{v}");
}

#[test]
fn a_judge_that_gives_no_answer_changes_nothing() {
    let tmp = seeded();
    // No rule matches and no "*": every question goes unanswered, exactly
    // like a provider that dropped them.
    let v = recall(
        tmp.path(),
        "Where does Noemi Varkoni work?",
        Some("on"),
        Some(r#"{"no passage says this": 0.01}"#),
    );
    assert_eq!(v["data"]["verdict"], "results", "{v}");
    assert_eq!(contents(&v).len(), 2, "{v}");
    assert!(v["data"]["results"][0]["relevance"].is_null(), "{v}");
    assert_eq!(v["metadata"]["relevance"]["judged"], 0, "{v}");
}

#[test]
fn off_or_no_judge_keeps_todays_recall() {
    let tmp = seeded();
    // No key and no stub: the default-on site has no judge to ask.
    let v = recall(tmp.path(), "What is Rosalie's blood type?", None, None);
    assert_eq!(v["data"]["verdict"], "results", "{v}");
    assert!(v["metadata"].get("relevance").is_none(), "{v}");
    // Off never asks, even with a judge available.
    let v = recall(
        tmp.path(),
        "What is Rosalie's blood type?",
        Some("off"),
        Some("0.01"),
    );
    assert_eq!(v["data"]["verdict"], "results", "{v}");
    assert!(v["metadata"].get("relevance").is_none(), "{v}");
}

#[test]
fn the_judge_sees_the_memory_date() {
    let tmp = store();
    run_json(
        &mut engram(tmp.path()),
        &[
            "remember",
            ELIAS,
            "--kb",
            "personal",
            "--no-extract-facts",
            "--event-time",
            "2025-02-14",
        ],
    );
    // The rule matches only if the date rides in the instructions.
    let v = recall(
        tmp.path(),
        "Where did Elias keep his gear in February?",
        Some("on"),
        Some(r#"{"2025-02-14": 0.9, "*": 0.01}"#),
    );
    assert_eq!(v["data"]["verdict"], "results", "{v}");
    assert_eq!(v["data"]["results"][0]["relevance"].as_f64(), Some(0.9));
}

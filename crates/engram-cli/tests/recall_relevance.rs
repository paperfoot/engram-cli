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
            &[
                "remember",
                content,
                "--kb",
                "personal",
                "--no-extract-facts",
            ],
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
    let v = recall(
        tmp.path(),
        "What is Rosalie's blood type?",
        None,
        Some("0.01"),
    );
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

/// An OpenRouter key was set for extraction; it is not consent to send
/// recalled passages to TypeSafe. Only a TypeSafe key turns the default on,
/// and `config show` says which default applies.
#[test]
fn only_a_typesafe_key_turns_relevance_on_by_default() {
    let tmp = store();
    let show = |env: (&str, &str)| {
        let mut cmd = engram(tmp.path());
        cmd.env(env.0, env.1);
        run_json(&mut cmd, &["config", "show"])
    };
    let v = show(("OPENROUTER_API_KEY", "sk-or-test"));
    let j = &v["data"]["judge"];
    assert_eq!(j["relevance"], "off", "{v}");
    assert!(
        j["relevance_source"]
            .as_str()
            .unwrap()
            .starts_with("default off"),
        "{v}"
    );
    let v = show(("TYPESAFE_API_KEY", "ts-test"));
    let j = &v["data"]["judge"];
    assert_eq!(j["relevance"], "on", "{v}");
    assert!(
        j["relevance_source"]
            .as_str()
            .unwrap()
            .starts_with("default on"),
        "{v}"
    );
    // An explicit setting opts in through the OpenRouter route.
    let mut cmd = engram(tmp.path());
    cmd.env("OPENROUTER_API_KEY", "sk-or-test")
        .env("ENGRAM_JUDGE_RELEVANCE", "shadow");
    let v = run_json(&mut cmd, &["config", "show"]);
    assert_eq!(v["data"]["judge"]["relevance"], "shadow", "{v}");
    assert_eq!(
        v["data"]["judge"]["relevance_source"], "set by ENGRAM_JUDGE_RELEVANCE",
        "{v}"
    );
}

/// `--top-k 1` with an irrelevant first candidate and the answer second.
/// The cut to top_k used to happen before the judge, so the one result was
/// dropped and the recall said `no_match` with the answer in the pool.
#[test]
fn the_judge_filters_before_the_top_k_cut() {
    let tmp = seeded();
    let mut cmd = engram(tmp.path());
    cmd.env("ENGRAM_JUDGE_RELEVANCE", "on")
        .env("ENGRAM_JUDGE_STUB", r#"{"climbing gear": 0.9, "*": 0.01}"#);
    let v = run_json(
        &mut cmd,
        &[
            "recall",
            "Noemi Varkoni Halden Labs Ghent climbing gear",
            "--top-k",
            "1",
        ],
    );
    assert_eq!(v["data"]["verdict"], "results", "{v}");
    assert_eq!(contents(&v), vec![ELIAS.to_string()], "{v}");
}

/// `bench personal --profile offline` promises no network. It resolved the
/// judge as if online, so a configured TypeSafe key sent every query and
/// its passages out and made the offline suite nondeterministic. The proxy
/// points at a dead port so a regression fails fast instead of calling out.
#[test]
fn the_offline_benchmark_never_reaches_a_judge() {
    let tmp = store();
    let mut cmd = engram(tmp.path());
    cmd.env_remove("ENGRAM_BENCH_FORCE_STUB")
        .env("TYPESAFE_API_KEY", "ts-test-not-a-key")
        .env("ENGRAM_JUDGE_RELEVANCE", "on")
        .env("HTTPS_PROXY", "http://127.0.0.1:9")
        .env("HTTP_PROXY", "http://127.0.0.1:9")
        .env("ALL_PROXY", "http://127.0.0.1:9");
    let v = run_json(&mut cmd, &["bench", "personal", "--profile", "offline"]);
    assert_eq!(v["data"]["profile"], "offline", "{v}");
    assert_eq!(v["metadata"]["relevance_judge"], "off", "{v}");
}

/// `--top-k 10` with ten irrelevant candidates and the answer eleventh. A
/// fixed pool of max(top_k, 10) judged the ten misses, dropped them and
/// abstained with the answer one rank away. Recall now refills down the
/// reranked pool until top_k survive or the pool is exhausted.
#[test]
fn the_judge_refills_past_a_full_page_of_misses() {
    let tmp = store();
    for i in 0..10 {
        run_json(
            &mut engram(tmp.path()),
            &[
                "remember",
                &format!("Noemi Varkoni note {i}: Halden Labs in Ghent, meeting {i}."),
                "--kb",
                "personal",
                "--no-extract-facts",
            ],
        );
    }
    run_json(
        &mut engram(tmp.path()),
        &["remember", ELIAS, "--kb", "personal", "--no-extract-facts"],
    );
    let mut cmd = engram(tmp.path());
    cmd.env("ENGRAM_JUDGE_RELEVANCE", "on")
        .env("ENGRAM_JUDGE_STUB", r#"{"climbing gear": 0.9, "*": 0.01}"#);
    let v = run_json(
        &mut cmd,
        &[
            "recall",
            "Noemi Varkoni Halden Labs Ghent meeting",
            "--top-k",
            "10",
        ],
    );
    assert_eq!(v["data"]["verdict"], "results", "{v}");
    assert_eq!(contents(&v), vec![ELIAS.to_string()], "{v}");
}

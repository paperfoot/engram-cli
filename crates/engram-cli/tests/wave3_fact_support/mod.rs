//! Shared harness for the wave-3 fact tests.
//!
//! Isolation is `ENGRAM_STORE` pointed at a TempDir and nothing else — no HOME
//! rewriting, so a leaked path cannot reach the developer's real store
//! (interfaces.md §5). Every key is removed from the environment, so a test
//! that accidentally reaches for a network provider fails loudly instead of
//! spending money.

#![allow(dead_code)]

use assert_cmd::Command;
use serde_json::Value;

pub fn store() -> tempfile::TempDir {
    tempfile::tempdir().expect("tmp store")
}

pub fn engram(store: &std::path::Path) -> Command {
    let mut cmd = Command::cargo_bin("engram").expect("binary built");
    cmd.env("ENGRAM_STORE", store);
    cmd.env("ENGRAM_BENCH_FORCE_STUB", "1");
    cmd.env_remove("GEMINI_API_KEY");
    cmd.env_remove("COHERE_API_KEY");
    cmd.env_remove("OPENROUTER_API_KEY");
    cmd.env_remove("TYPESAFE_API_KEY");
    cmd.env_remove("ENGRAM_JUDGE_STUB");
    cmd.env_remove("ENGRAM_JUDGE_GROUNDING");
    cmd.env_remove("ENGRAM_JUDGE_ADJUDICATE");
    cmd.env_remove("ENGRAM_JUDGE_DEDUPE");
    cmd.env_remove("ENGRAM_FACT_LLM_STUB");
    cmd.env_remove("ENGRAM_FACT_WRITE_FAIL");
    cmd.env_remove("ENGRAM_FACT_QUEUE_ALL");
    cmd
}

pub fn parse_stdout(out: &[u8]) -> Value {
    serde_json::from_slice(out).expect("valid JSON on stdout")
}

pub fn run_json(cmd: &mut Command, args: &[&str]) -> Value {
    let out = cmd
        .args(args)
        .arg("--json")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    parse_stdout(&out)
}

/// `remember` with caller-asserted triples: no LLM, no key, and the same
/// planner every extracted fact goes through.
pub fn remember_facts(store: &std::path::Path, content: &str, facts_json: &str) -> Value {
    run_json(
        &mut engram(store),
        &[
            "remember",
            content,
            "--kb",
            "personal",
            "--facts-json",
            facts_json,
        ],
    )
}

pub fn facts_for(store: &std::path::Path, subject: &str) -> Vec<Value> {
    run_json(
        &mut engram(store),
        &["facts", "list", "--subject", subject, "--kb", "personal"],
    )["data"]["facts"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// Active AND historic, so a closed interval is still visible.
pub fn facts_show(store: &std::path::Path, subject: &str) -> Value {
    run_json(
        &mut engram(store),
        &["facts", "show", subject, "--kb", "personal", "--diary", "*"],
    )
}

/// Every row `facts show` knows about, whichever bucket it puts it in.
///
/// `show` still splits active from historic on `superseded_by IS NULL`, so a
/// row closed with `reason: stopped` — which has no successor — lands in the
/// ACTIVE list. That is the `is_open_fact()` sweep, a named wave-3 integration
/// gate owned by read-surfaces and storage (CD-2), not this lane's to fix
/// here. These tests therefore assert on the STORED shape (`valid_until`,
/// `resolution`) and let the bucket be whatever the unswept reader says; the
/// one test that asserts the split is `#[ignore]`d and names the gate.
pub fn all_known_facts(store: &std::path::Path, subject: &str) -> Vec<Value> {
    let shown = facts_show(store, subject);
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

/// The rows a reader must treat as CURRENT: interval-open, whatever closed
/// them. `facts list` is already interval-aware.
pub fn current_facts(store: &std::path::Path, subject: &str) -> Vec<Value> {
    facts_for(store, subject)
}

/// The rows whose interval has ended.
pub fn closed_facts(store: &std::path::Path, subject: &str) -> Vec<Value> {
    all_known_facts(store, subject)
        .into_iter()
        .filter(|f| !f["valid_until"].is_null())
        .collect()
}

pub fn open_conflicts(store: &std::path::Path) -> Value {
    run_json(&mut engram(store), &["facts", "conflicts", "--open"])
}

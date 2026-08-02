//! Shared harness for the wave-5 reasoning-layer tests.
//!
//! Isolation is `ENGRAM_STORE` on a TempDir and nothing else: no HOME
//! rewriting and no XDG tricks, so a leaked path in this suite cannot reach
//! the developer's real store, which is the whole of CD-6. Every
//! provider key is removed, so nothing here can make a paid call or pass only
//! on the machine of whoever exported the right key.
//!
//! A subdirectory rather than a bare file, so cargo does not compile it as its
//! own (empty) test target.

#![allow(dead_code)]

use assert_cmd::Command;
use serde_json::Value;
use tempfile::TempDir;

pub fn store() -> TempDir {
    tempfile::tempdir().expect("tmp store")
}

pub fn engram(store: &std::path::Path) -> Command {
    let mut cmd = Command::cargo_bin("engram").expect("binary built");
    cmd.env("ENGRAM_STORE", store);
    cmd.env("ENGRAM_BENCH_FORCE_STUB", "1");
    for key in [
        "ENGRAM_HOME",
        "GEMINI_API_KEY",
        "GEMINI_API_KEY_FALLBACKS",
        "COHERE_API_KEY",
        "OPENROUTER_API_KEY",
        "ENGRAM_FACT_LLM_STUB",
        "ENGRAM_COMPILER_LLM_STUB",
        "ENGRAM_FORCE_TTY",
    ] {
        cmd.env_remove(key);
    }
    cmd
}

pub fn run_json(store: &std::path::Path, args: &[&str]) -> Value {
    let out = engram(store)
        .args(args)
        .arg("--json")
        .output()
        .expect("engram ran");
    assert!(
        out.status.success(),
        "`engram {}` failed ({:?}):\n{}",
        args.join(" "),
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "stdout is not JSON ({e}):\n{}",
            String::from_utf8_lossy(&out.stdout)
        )
    })
}

/// Raw stdout, for the tests that assert stdout carries exactly ONE JSON
/// document — the regression test for a command that prints twice.
pub fn run_stdout(store: &std::path::Path, args: &[&str]) -> String {
    let out = engram(store)
        .args(args)
        .arg("--json")
        .output()
        .expect("engram ran");
    assert!(
        out.status.success(),
        "`engram {}` failed:\n{}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).to_string()
}

/// Run and expect a failure; returns (exit code, parsed error envelope).
pub fn run_err(store: &std::path::Path, args: &[&str]) -> (i32, Value) {
    let out = engram(store)
        .args(args)
        .arg("--json")
        .output()
        .expect("engram ran");
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    let line = stderr
        .lines()
        .rev()
        .find(|l| l.trim_start().starts_with('{'))
        .unwrap_or_else(|| panic!("no JSON error envelope on stderr:\n{stderr}"));
    (
        out.status.code().unwrap_or(-1),
        serde_json::from_str(line).expect("parseable error envelope"),
    )
}

/// Store caller-asserted triples through the real write path: no LLM, no key,
/// the same planner an extracted fact goes through.
pub fn remember_facts(store: &std::path::Path, kb: &str, content: &str, facts_json: &str) -> Value {
    run_json(
        store,
        &[
            "remember",
            content,
            "--kb",
            kb,
            "--offline",
            "--facts-json",
            facts_json,
        ],
    )
}

pub fn state(store: &std::path::Path, subject: &str, extra: &[&str]) -> Value {
    let mut args = vec!["state", subject];
    args.extend_from_slice(extra);
    run_json(store, &args)
}

/// The values of one relation group, or an empty vector when the group is
/// absent — "no such relation" and "relation with no values" read the same to
/// a caller, and both are correct answers to "what is true now".
pub fn group_values(v: &Value, relation: &str) -> Vec<Value> {
    v["data"]["state"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .find(|g| g["relation"] == relation)
        .map(|g| g["values"].as_array().cloned().unwrap_or_default())
        .unwrap_or_default()
}

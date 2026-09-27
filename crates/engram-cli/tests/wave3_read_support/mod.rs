//! Shared harness for the wave-3 read-surface tests.
//!
//! Isolation is `ENGRAM_STORE` on a TempDir and nothing else: no HOME
//! rewriting, no XDG tricks, so a leaked path cannot reach the developer's
//! real store. Every provider key is cleared, so no test here can make a paid
//! call or pass only on the machine of whoever exported the right key.
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
        "GEMINI_API_KEY",
        "GEMINI_API_KEY_FALLBACKS",
        "COHERE_API_KEY",
        "OPENROUTER_API_KEY",
        "TYPESAFE_API_KEY",
        "ENGRAM_JUDGE_STUB",
        "ENGRAM_JUDGE_GROUNDING",
        "ENGRAM_JUDGE_ADJUDICATE",
        "ENGRAM_JUDGE_DEDUPE",
        "ENGRAM_JUDGE_LOG",
        "ENGRAM_COMPILER_LLM_STUB",
        "ENGRAM_FACT_LLM_STUB",
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

/// Store one memory, offline, without invoking any extractor.
pub fn remember(store: &std::path::Path, content: &str, kb: &str) -> String {
    let v = run_json(
        store,
        &[
            "remember",
            content,
            "--kb",
            kb,
            "--offline",
            "--no-extract-facts",
        ],
    );
    v["data"]["id"].as_str().expect("memory id").to_string()
}

/// Store one caller-supplied triple, offline. `--autonomy strict` so a
/// contradiction queues instead of being auto-adjudicated.
pub fn remember_fact(
    store: &std::path::Path,
    note: &str,
    kb: &str,
    diary: &str,
    subject: &str,
    predicate: &str,
    object: &str,
) -> Value {
    let facts = format!(
        r#"[{{"subject":"{subject}","predicate":"{predicate}","object":"{object}","confidence":1.0}}]"#
    );
    run_json(
        store,
        &[
            "remember",
            note,
            "--kb",
            kb,
            "--diary",
            diary,
            "--offline",
            "--no-extract-facts",
            "--autonomy",
            "strict",
            "--facts-json",
            &facts,
        ],
    )
}

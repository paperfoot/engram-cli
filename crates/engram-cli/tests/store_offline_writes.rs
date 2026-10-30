//! [C8] `ingest` and `chunks repair` take the write side's key rule.
//!
//! Both embedded with the 64-dim stub whenever no Gemini key resolved, and said
//! nothing: default recall then refused the store ("embedding metadata
//! mismatch"). Keyless use is now the explicit `--offline`, as on `remember`.
//!
//! Isolation: HOME/XDG and ENGRAM_STORE point into a TempDir, so neither a
//! developer's config file nor their key can be reached, and every provider
//! key and the stub hook are removed from the environment.

use assert_cmd::Command;
use serde_json::Value;
use std::path::Path;

const NOTE: &str = "Ana keeps her sailing logs in a notebook by the door.";

fn keyless(home: &Path) -> Command {
    let mut cmd = Command::cargo_bin("engram").expect("binary built");
    for k in [
        "ENGRAM_BENCH_FORCE_STUB",
        "GEMINI_API_KEY",
        "COHERE_API_KEY",
        "OPENROUTER_API_KEY",
        "ANTHROPIC_API_KEY",
        "TYPESAFE_API_KEY",
        "GEMINI_API_KEY_FALLBACKS",
        "ENGRAM_JUDGE_STUB",
        "ENGRAM_JUDGE_GROUNDING",
        "ENGRAM_JUDGE_ADJUDICATE",
        "ENGRAM_JUDGE_DEDUPE",
        "ENGRAM_JUDGE_LOG",
        "ENGRAM_EMBED_FAIL",
    ] {
        cmd.env_remove(k);
    }
    cmd.env("ENGRAM_STORE", home.join("store"));
    cmd.env("HOME", home);
    cmd.env("XDG_CONFIG_HOME", home.join(".config"));
    cmd.env("XDG_DATA_HOME", home.join(".local/share"));
    cmd.env("XDG_CACHE_HOME", home.join(".cache"));
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

/// Runs a command that must be refused for want of a key; returns the error
/// envelope (the last `{`-line on stderr).
fn refused(cmd: &mut Command, args: &[&str]) -> Value {
    let assert = cmd.args(args).arg("--json").assert().failure().code(2);
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).to_string();
    let line = stderr
        .lines()
        .rev()
        .find(|l| l.trim_start().starts_with('{'))
        .expect("JSON error on stderr");
    let v: Value = serde_json::from_str(line).expect("parseable error envelope");
    assert_eq!(v["error"]["code"], "config_error", "{v}");
    let msg = v["error"]["message"].as_str().unwrap_or_default();
    assert!(msg.contains("--offline"), "must name the remedy: {msg}");
    v
}

fn counts(home: &Path) -> Value {
    run_json(&mut keyless(home), &["export", "--format", "json"])["data"]["counts"].clone()
}

#[test]
fn a_keyless_ingest_refuses_and_writes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let note = tmp.path().join("note.md");
    std::fs::write(&note, NOTE).unwrap();

    let err = refused(
        &mut keyless(tmp.path()),
        &["ingest", note.to_str().unwrap(), "--kb", "personal"],
    );
    let msg = err["error"]["message"].as_str().unwrap_or_default();
    assert!(msg.contains("engram ingest"), "{msg}");

    let c = counts(tmp.path());
    assert_eq!(c["memories"], 0, "{c}");
    assert_eq!(c["chunks"], 0, "{c}");
}

#[test]
fn an_offline_ingest_records_the_stub_metadata_remember_offline_does() {
    let tmp = tempfile::tempdir().unwrap();
    let note = tmp.path().join("note.md");
    std::fs::write(&note, NOTE).unwrap();

    let v = run_json(
        &mut keyless(tmp.path()),
        &[
            "ingest",
            note.to_str().unwrap(),
            "--kb",
            "personal",
            "--offline",
        ],
    );
    assert_eq!(v["data"]["chunks_created"], 1, "{v}");
    assert_eq!(v["metadata"]["embedder"], "stub-64", "{v}");
    assert_eq!(v["metadata"]["embed_dimensions"], 64, "{v}");

    run_json(
        &mut keyless(tmp.path()),
        &[
            "remember",
            "The sailing club meets on Thursdays.",
            "--kb",
            "personal",
            "--no-extract-facts",
            "--offline",
        ],
    );

    // The recall guard refuses a scope holding two kinds of embedding
    // metadata, so one offline read over both writes proves they match.
    let found = run_json(
        &mut keyless(tmp.path()),
        &[
            "recall",
            "sailing logs notebook",
            "--kb",
            "personal",
            "--profile",
            "offline",
            "--full",
        ],
    );
    let results = found["data"]["results"].as_array().expect("results");
    assert!(
        results.iter().any(|r| r["content"] == NOTE),
        "the ingested note is searchable: {found}"
    );
}

#[test]
fn a_keyless_chunks_repair_refuses_and_offline_repairs() {
    let tmp = tempfile::tempdir().unwrap();
    // A floor of 1 stores the bare date lines as chunks of their own: the
    // degenerate shape `chunks repair` exists to merge.
    let mut doc = String::new();
    for day in 16..20 {
        doc.push_str(&format!(
            "Sep {day}, 2025\n\nMet Noemi at the Belgrade summit on day {day}; she throws a \
             party every year at the family house outside the city.\n\n"
        ));
    }
    run_json(
        keyless(tmp.path())
            .env("ENGRAM_CHUNK_MIN_CHARS", "1")
            .env("ENGRAM_CHUNK_TARGET_CHARS", "1"),
        &[
            "remember",
            &doc,
            "--kb",
            "legacy",
            "--no-extract-facts",
            "--offline",
        ],
    );
    let before = counts(tmp.path());

    let err = refused(
        &mut keyless(tmp.path()),
        &["chunks", "repair", "--kb", "legacy"],
    );
    let msg = err["error"]["message"].as_str().unwrap_or_default();
    assert!(msg.contains("engram chunks repair"), "{msg}");
    assert_eq!(counts(tmp.path()), before, "a refused repair wrote nothing");

    let v = run_json(
        &mut keyless(tmp.path()),
        &["chunks", "repair", "--kb", "legacy", "--offline"],
    );
    assert!(
        v["data"]["chunks_reembedded"].as_u64().unwrap_or(0) > 0,
        "{v}"
    );
    assert_eq!(v["data"]["embedder"], "stub-64", "{v}");
}

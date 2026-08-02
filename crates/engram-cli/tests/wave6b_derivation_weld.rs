//! Wave 6b — the extraction marker is welded like every other projection.
//!
//! `fact_extraction_log` is what retires a memory from the backfill queue, and
//! normal extraction wrote it with `record_fact_extraction` — a bare mutation
//! with no event behind it. `admin rebuild` deletes the derivation tables and
//! refills them from `derivation.ran`, so the marker vanished, the memory
//! reappeared in the queue, and the next sweep paid a provider for a call the
//! store had already made and thrown away. Invariant 1 has no exceptions [G6].
//!
//! Isolation is `ENGRAM_STORE` on a TempDir and nothing else.

use assert_cmd::prelude::*;
use serde_json::Value;
use std::process::Command;
use tempfile::TempDir;

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

/// `remember` with an extraction model behind it, scripted offline.
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

fn pending(store: &std::path::Path) -> u64 {
    run_json(
        &mut engram(store),
        &["facts", "backfill", "--kb", "personal", "--dry-run"],
    )["data"]["pending"]
        .as_u64()
        .expect("a pending count")
}

/// THE BLOCKER. A memory the extractor read and found nothing in earns a
/// marker; a rebuild used to throw that marker away.
#[test]
fn an_extraction_marker_survives_a_rebuild() {
    let tmp = TempDir::new().unwrap();
    remember_extracted(
        tmp.path(),
        "No extractable atomic claim here.",
        r#"{"*":[]}"#,
    );
    assert_eq!(
        pending(tmp.path()),
        0,
        "the extractor has read this memory; nothing is pending"
    );

    engram(tmp.path())
        .args(["admin", "rebuild", "--confirm", "--json"])
        .assert()
        .success();

    assert_eq!(
        pending(tmp.path()),
        0,
        "a rebuild replays the journal; a marker it cannot recreate is a \
         provider call the store pays for twice"
    );
}

/// The same weld on the path that DOES produce facts: the marker and the
/// facts survive together, so a rebuilt store neither re-extracts nor loses
/// what the extraction found.
#[test]
fn a_productive_extraction_and_its_marker_both_survive_a_rebuild() {
    let tmp = TempDir::new().unwrap();
    remember_extracted(
        tmp.path(),
        "Bruno joined Acme.",
        r#"{"*":[{"subject":"Bruno","predicate":"works_at","object":"Acme"}]}"#,
    );
    assert_eq!(pending(tmp.path()), 0);
    let before = facts_for(tmp.path(), "Bruno").len();
    assert_eq!(before, 1);

    engram(tmp.path())
        .args(["admin", "rebuild", "--confirm", "--json"])
        .assert()
        .success();

    assert_eq!(pending(tmp.path()), 0);
    assert_eq!(facts_for(tmp.path(), "Bruno").len(), before);
}

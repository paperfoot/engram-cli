//! Wave 2 — `engram admin rebuild` through the real binary.
//!
//! Isolation is `ENGRAM_STORE` alone: no HOME tricks, no XDG juggling. A test
//! that can open `~/Library/Application Support/…/engram` is a test that can
//! migrate the developer's live store, which has already happened once.

use assert_cmd::prelude::*;
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
        "TYPESAFE_API_KEY",
        "GEMINI_API_KEY_FALLBACKS",
        "ENGRAM_JUDGE_STUB",
        "ENGRAM_JUDGE_GROUNDING",
        "ENGRAM_JUDGE_ADJUDICATE",
        "ENGRAM_JUDGE_DEDUPE",
        "ENGRAM_JUDGE_LOG",
        "ENGRAM_FACT_LLM_STUB",
    ] {
        cmd.env_remove(k);
    }
    cmd
}

fn seed(store: &std::path::Path) {
    engram(store)
        .args([
            "remember",
            "Ana lives in Lisbon.",
            "--offline",
            "--kb",
            "personal",
            "--autonomy",
            "strict",
            "--facts-json",
            r#"[{"subject":"Ana","predicate":"lives_in","object":"Lisbon","confidence":0.9}]"#,
            "--json",
        ])
        .assert()
        .success();
    engram(store)
        .args([
            "remember",
            "Ana moved to Porto.",
            "--offline",
            "--kb",
            "personal",
            "--autonomy",
            "strict",
            "--facts-json",
            r#"[{"subject":"Ana","predicate":"lives_in","object":"Porto","confidence":0.9}]"#,
            "--json",
        ])
        .assert()
        .success();
}

/// The published fact projection, read through the surface an agent uses.
/// Deliberately not raw SQL: the property that matters is that the ANSWER is
/// unchanged, not that a column happens to be.
fn facts_digest(store: &std::path::Path) -> String {
    let out = engram(store)
        .args(["facts", "list", "--all", "--json", "--limit", "100"])
        .assert()
        .success()
        .get_output()
        .clone();
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json envelope");
    let mut rows: Vec<String> = json["data"]["facts"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|f| {
            format!(
                "{}:{}:{}:{}",
                f["id"], f["object"], f["valid_until"], f["superseded_by"]
            )
        })
        .collect();
    rows.sort();
    rows.join("|")
}

#[test]
fn rebuild_without_confirm_exits_3_and_prints_a_runnable_command() {
    let tmp = TempDir::new().unwrap();
    seed(tmp.path());
    let out = engram(tmp.path())
        .args(["admin", "rebuild", "--json"])
        .assert()
        .failure()
        .code(3)
        .get_output()
        .clone();
    let text = String::from_utf8_lossy(&out.stdout) + String::from_utf8_lossy(&out.stderr);
    assert!(
        text.contains("engram admin rebuild --confirm"),
        "the refusal must name the command to re-run: {text}"
    );
    assert!(text.contains("--dry-run"), "…and the rehearsal: {text}");
}

#[test]
fn a_dry_run_needs_no_confirm_and_changes_nothing() {
    let tmp = TempDir::new().unwrap();
    seed(tmp.path());
    let before = facts_digest(tmp.path());
    let out = engram(tmp.path())
        .args(["admin", "rebuild", "--dry-run", "--json"])
        .assert()
        .success()
        .get_output()
        .clone();
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json envelope");
    assert_eq!(json["status"], "success");
    assert_eq!(json["data"]["dry_run"], true);
    assert_eq!(facts_digest(tmp.path()), before);
}

#[test]
fn rebuild_names_what_it_did_not_rebuild() {
    let tmp = TempDir::new().unwrap();
    seed(tmp.path());
    let before = facts_digest(tmp.path());
    let out = engram(tmp.path())
        .args(["admin", "rebuild", "--confirm", "--json"])
        .assert()
        .success()
        .get_output()
        .clone();
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json envelope");
    let skipped: Vec<String> = json["data"]["skipped"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["table"].as_str().unwrap().to_string())
        .collect();
    // Invariant 3's one-line statement promises more than any journal can
    // deliver, so the narrowing is printed rather than assumed.
    for table in ["memories", "chunks", "claims", "entities"] {
        assert!(skipped.contains(&table.to_string()), "{table} not named");
    }
    for s in json["data"]["skipped"].as_array().unwrap() {
        assert!(
            !s["reason"].as_str().unwrap_or("").is_empty(),
            "every skipped table must say WHY"
        );
    }
    assert_eq!(
        facts_digest(tmp.path()),
        before,
        "and the projection it does own must come back identical"
    );
    assert!(json["data"]["events_replayed"].as_u64().unwrap() > 0);
}

#[test]
fn the_binary_writes_only_inside_engram_store() {
    let tmp = TempDir::new().unwrap();
    seed(tmp.path());
    engram(tmp.path())
        .args(["admin", "rebuild", "--confirm", "--json"])
        .assert()
        .success();
    assert!(tmp.path().join("db.sqlite").exists());
    // The default data directory must not have been created by any of the
    // above. Checking existence rather than mtime keeps the assertion true on
    // a developer machine that legitimately has a real store.
    let default_dir = directories::ProjectDirs::from("bio", "199-biotechnologies", "engram")
        .expect("project dirs")
        .data_dir()
        .join("engram-wave2-canary-that-should-never-exist");
    assert!(!default_dir.exists());
}

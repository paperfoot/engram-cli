//! [V5] Which rule or judge closed an interval survives `admin rebuild`.
//!
//! `facts.resolution` was written to the projection alone, so a rebuild from
//! the journal erased it: "Lisbon was closed by slot_lww" came back as a close
//! nobody could attribute. The close event now carries the tag.
//!
//! Isolation is `ENGRAM_STORE` alone, and every provider key is removed.

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
    ] {
        cmd.env_remove(k);
    }
    cmd
}

fn run_json(store: &std::path::Path, args: &[&str]) -> Value {
    let out = engram(store)
        .args(args)
        .arg("--json")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    serde_json::from_slice(&out).expect("json envelope")
}

fn remember(store: &std::path::Path, content: &str, object: &str) {
    let facts = format!(r#"[{{"subject":"Ana","predicate":"lives_in","object":"{object}"}}]"#);
    run_json(
        store,
        &[
            "remember",
            content,
            "--offline",
            "--kb",
            "personal",
            "--facts-json",
            &facts,
        ],
    );
}

fn lisbon(store: &std::path::Path) -> Value {
    run_json(store, &["facts", "list", "--all", "--limit", "100"])["data"]["facts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["object"] == "Lisbon")
        .cloned()
        .expect("the Lisbon row is still listed")
}

#[test]
fn a_slot_lww_close_keeps_its_resolution_through_admin_rebuild() {
    let tmp = TempDir::new().unwrap();
    remember(tmp.path(), "Ana lives in Lisbon.", "Lisbon");
    remember(tmp.path(), "Ana moved to Porto.", "Porto");

    let before = lisbon(tmp.path());
    assert!(
        !before["valid_until"].is_null(),
        "Porto closed it: {before}"
    );
    assert_eq!(before["resolution"], "slot_lww", "{before}");

    run_json(tmp.path(), &["admin", "rebuild", "--confirm"]);

    let after = lisbon(tmp.path());
    assert_eq!(
        after["resolution"], "slot_lww",
        "rebuild erased it: {after}"
    );
    assert_eq!(after["close_authority"], before["close_authority"]);
    assert_eq!(after["valid_until"], before["valid_until"]);
}

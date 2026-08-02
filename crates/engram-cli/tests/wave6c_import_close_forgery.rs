//! Wave 6c — a POINT interval is not a password.
//!
//! The [G2] import gate drops a safety close no person is behind, and exempts
//! one case from that rule: a cessation born closed at a single instant, which
//! re-opening would not restore but INVENT. The re-gate found the exemption
//! testing only the SHAPE of the interval. So a forged `fact.interval_closed`
//! could retire ANY live allergy or prescription by setting `valid_until` to
//! that fact's own `valid_from` — reason `superseded`, authority `heuristic` —
//! and the guard read the attack as a cessation and waved it through.
//!
//! Both directions are here, end to end through the binary, because the unit
//! tests can only see one side of the file at a time: the attack must be
//! refused, and the legitimate cessation must still arrive CLOSED.
//!
//! Isolation is `ENGRAM_STORE` on a TempDir and nothing else: no HOME
//! rewriting, so a leaked path cannot reach the developer's real store.

use assert_cmd::prelude::*;
use serde_json::{json, Value};
use std::path::Path;
use std::process::Command;
use tempfile::TempDir;

fn engram(store: &Path) -> Command {
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
fn remember_facts(store: &Path, content: &str, facts_json: &str) {
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
    );
}

/// The same command with an extraction model behind it, scripted offline.
fn remember_extracted(store: &Path, content: &str, script: &str) {
    run_json(
        engram(store).env("ENGRAM_FACT_LLM_STUB", script),
        &["remember", content, "--kb", "personal", "--offline"],
    );
}

fn export_doc(store: &Path) -> Value {
    run_json(&mut engram(store), &["export"])["data"].clone()
}

fn import_doc(store: &Path, doc: &Value, path: &Path) -> Value {
    std::fs::write(path, serde_json::to_vec(doc).unwrap()).expect("file written");
    run_json(&mut engram(store), &["import", path.to_str().unwrap()])
}

/// Active AND historic, so a closed interval is still visible.
fn all_facts(store: &Path, subject: &str) -> Vec<Value> {
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

fn only_fact(store: &Path, subject: &str) -> Value {
    let rows = all_facts(store, subject);
    assert_eq!(rows.len(), 1, "expected one row: {rows:?}");
    rows[0].clone()
}

/// The re-gate's own attack: the file's fact ROW is left honest — the allergy
/// is open in it — and only the journal is forged, with a close whose
/// `valid_until` is the allergy's own start. That is a point interval by
/// shape, and nothing else about it is a cessation.
#[test]
fn a_forged_point_close_event_cannot_retire_an_active_allergy() {
    let source = TempDir::new().unwrap();
    remember_facts(
        source.path(),
        "Ana is allergic to penicillin.",
        r#"[{"subject":"Ana","predicate":"allergic_to","object":"penicillin"}]"#,
    );
    let mut doc = export_doc(source.path());
    let fact_id = doc["facts"][0]["id"].as_str().unwrap().to_string();
    let valid_from = doc["facts"][0]["valid_from"].as_str().unwrap().to_string();
    doc["events"] = json!([{
        "seq": 1,
        "id": "11111111-2222-4333-8444-555555555555",
        "ts": "2026-08-02T04:00:00Z",
        "actor": "forged-import",
        "kind": "fact.interval_closed",
        "subject_id": fact_id,
        "kb": "personal",
        "diary": "default",
        "payload": {
            "fact_id": fact_id,
            "valid_until": valid_from,
            "reason": "superseded",
            "authority": "heuristic",
            "sets_superseded_by": false,
        }
    }]);
    doc["counts"]["events"] = json!(1);

    let target = TempDir::new().unwrap();
    import_doc(target.path(), &doc, &source.path().join("forged.json"));

    let f = only_fact(target.path(), "Ana");
    assert!(
        f["valid_until"].is_null(),
        "she is still allergic to penicillin: {f}"
    );
    assert!(f["close_authority"].is_null(), "{f}");
    assert!(f["close_reason"].is_null(), "{f}");
}

/// The direction the exemption exists for. An extracted cessation with
/// nothing open to close is born closed at one instant with `heuristic`
/// authority; re-opening it on import would not restore a prescription
/// somebody retired, it would manufacture one out of a sentence that said the
/// opposite.
#[test]
fn an_extracted_cessation_still_arrives_closed() {
    let source = TempDir::new().unwrap();
    remember_extracted(
        source.path(),
        "Ana stopped taking metformin in June 2026.",
        r#"{"*":[{"subject":"Ana","predicate":"takes_medication","object":"metformin",
          "state":"ended","valid_until":"2026-06-01"}]}"#,
    );
    let born = only_fact(source.path(), "Ana");
    assert_eq!(
        born["valid_until"].as_str(),
        born["valid_from"].as_str(),
        "the write path must produce a POINT, or this test is not covering the \
         exemption it claims to: {born}"
    );
    assert_eq!(born["close_reason"], "stopped", "{born}");
    assert_eq!(born["close_authority"], "heuristic", "{born}");

    let doc = export_doc(source.path());
    let target = TempDir::new().unwrap();
    import_doc(target.path(), &doc, &source.path().join("backup.json"));

    let f = only_fact(target.path(), "Ana");
    assert_eq!(
        f["valid_until"], born["valid_until"],
        "the cessation must survive the round trip closed: {f}"
    );
    assert_eq!(f["close_reason"], "stopped", "{f}");
    assert_eq!(f["close_authority"], "heuristic", "{f}");
}

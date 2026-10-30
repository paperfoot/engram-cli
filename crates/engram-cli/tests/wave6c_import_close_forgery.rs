//! Wave 6c — a safety interval never arrives closed from outside.
//!
//! Three gate passes found the same hole in the same place, and each fix
//! consulted one more attacker-controlled field. Pass 1: a non-person
//! `close_authority` closed a safety interval. Pass 2: a forged POINT shape
//! bypassed the authority check. Pass 3: a forged `reason: "stopped"` bypassed
//! the shape check, a forged row shadowed a legitimate event, and a refused
//! event was laundered into trusted history by `admin rebuild`.
//!
//! [G8] retires the whole apparatus. A fact on `takes_medication`,
//! `allergic_to` or `diagnosed_with` lands OPEN on every import path, and NO
//! field of the file is consulted to decide it — every one of them is written
//! by whoever wrote the file. [G9] makes it stick: events carry an `origin`
//! the file cannot set, so a refusal survives every later rebuild.
//!
//! End to end through the binary, because the unit tests can only see one side
//! of a file at a time.
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
        "ANTHROPIC_API_KEY",
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

/// [G8] The accepted cost, end to end, and the number that pays for it.
///
/// An extracted cessation is born closed at one instant. It imports OPEN now,
/// because `reason: "stopped"` is a string a forger types just as easily as
/// the planner writes it — and a rule that reads it is a rule the next file
/// satisfies. What the caller gets instead of silence is a count: the store
/// says how many closes it declined to honour.
#[test]
fn an_extracted_cessation_reopens_on_import_and_the_store_says_so() {
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
        "the write path must still produce a POINT, or this test is not \
         covering the case it claims to: {born}"
    );
    assert_eq!(born["close_reason"], "stopped", "{born}");

    let doc = export_doc(source.path());
    let target = TempDir::new().unwrap();
    let report = import_doc(target.path(), &doc, &source.path().join("backup.json"));

    let f = only_fact(target.path(), "Ana");
    assert!(
        f["valid_until"].is_null(),
        "a safety interval lands open however the file spells the close: {f}"
    );
    assert!(f["close_reason"].is_null(), "{f}");
    assert!(f["close_authority"].is_null(), "{f}");
    assert_eq!(
        report["data"]["safety_closes_refused"]["count"], 1,
        "the refusal must be SURFACED, not merely counted: {report}"
    );
}

/// A non-safety relation is untouched. Only three relations pay the fidelity
/// price, and a store that reopened every closed interval on import would be
/// unusable rather than cautious.
#[test]
fn an_ordinary_relation_still_imports_closed() {
    let source = TempDir::new().unwrap();
    // Two writes, not one: Lisbon has to be standing before Porto can retire
    // it, and a single batch asserts both at once.
    remember_facts(
        source.path(),
        "Bruno lives in Lisbon.",
        r#"[{"subject":"Bruno","predicate":"lives_in","object":"Lisbon"}]"#,
    );
    remember_facts(
        source.path(),
        "Bruno moved to Porto.",
        r#"[{"subject":"Bruno","predicate":"lives_in","object":"Porto"}]"#,
    );
    let doc = export_doc(source.path());
    let closed_before = all_facts(source.path(), "Bruno")
        .iter()
        .filter(|f| !f["valid_until"].is_null())
        .count();
    assert_eq!(closed_before, 1, "Lisbon closes when Porto arrives");

    let target = TempDir::new().unwrap();
    let report = import_doc(target.path(), &doc, &source.path().join("backup.json"));
    assert_eq!(
        report["data"]["safety_closes_refused"]["count"], 0,
        "{report}"
    );
    let closed_after = all_facts(target.path(), "Bruno")
        .iter()
        .filter(|f| !f["valid_until"].is_null())
        .count();
    assert_eq!(closed_after, 1, "Bruno really did move out of Lisbon");
}

/// [G9] The third gate's blocker, end to end. The honest open row and its
/// authentic event stay; ONE attacker-controlled close is appended. Import
/// refuses it — and so does the rebuild that reads the identical bytes back.
#[test]
fn a_refused_imported_close_stays_refused_through_admin_rebuild() {
    let source = TempDir::new().unwrap();
    remember_facts(
        source.path(),
        "Ana is allergic to penicillin.",
        r#"[{"subject":"Ana","predicate":"allergic_to","object":"penicillin"}]"#,
    );
    let mut doc = export_doc(source.path());
    let fact_id = doc["facts"][0]["id"].as_str().unwrap().to_string();
    let valid_from = doc["facts"][0]["valid_from"].as_str().unwrap().to_string();
    let next_seq = doc["events"].as_array().unwrap().len() + 1;
    doc["events"].as_array_mut().unwrap().push(json!({
        "seq": next_seq,
        "id": "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee",
        "ts": "2026-08-02T06:00:00Z",
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
    }));
    doc["counts"]["events"] = json!(next_seq);

    let target = TempDir::new().unwrap();
    import_doc(target.path(), &doc, &source.path().join("forged.json"));
    let after_import = only_fact(target.path(), "Ana");
    assert!(after_import["valid_until"].is_null(), "{after_import}");

    // The same bytes, replayed by the command that used to launder them.
    let rebuilt = run_json(
        &mut engram(target.path()),
        &["admin", "rebuild", "--confirm"],
    );
    let f = only_fact(target.path(), "Ana");
    assert!(
        f["valid_until"].is_null(),
        "an imported event is untrusted on EVERY replay, not just the first: {f}"
    );
    assert!(f["close_authority"].is_null(), "{f}");
    assert_eq!(
        rebuilt["data"]["close_authority_rejected"], 1,
        "and the rebuild reports the refusal rather than looking like a \
         rebuild that honoured it: {rebuilt}"
    );
}

/// The other side of [G9], and the one that proves the gate reads ORIGIN
/// rather than simply refusing everything: a store's OWN cessation, written
/// through the gated write path, survives its own `admin rebuild` closed.
#[test]
fn a_stores_own_cessation_survives_its_own_rebuild_closed() {
    let store = TempDir::new().unwrap();
    remember_extracted(
        store.path(),
        "Ana stopped taking metformin in June 2026.",
        r#"{"*":[{"subject":"Ana","predicate":"takes_medication","object":"metformin",
          "state":"ended","valid_until":"2026-06-01"}]}"#,
    );
    let born = only_fact(store.path(), "Ana");
    assert!(!born["valid_until"].is_null(), "{born}");

    let rebuilt = run_json(
        &mut engram(store.path()),
        &["admin", "rebuild", "--confirm"],
    );
    assert_eq!(rebuilt["data"]["close_authority_rejected"], 0, "{rebuilt}");
    let f = only_fact(store.path(), "Ana");
    assert_eq!(
        f["valid_until"], born["valid_until"],
        "a rebuild replays this store's history, it does not re-litigate it: {f}"
    );
    assert_eq!(f["close_reason"], "stopped", "{f}");
    assert_eq!(f["close_authority"], "heuristic", "{f}");
}

/// The row/event ordering trap. Rows landed before events and both used
/// `INSERT OR IGNORE`, so a forged row won every column and the authentic
/// observation beside it became a no-op. The journal decides now.
#[test]
fn a_forged_fact_row_cannot_shadow_the_authentic_event_beside_it() {
    let source = TempDir::new().unwrap();
    remember_facts(
        source.path(),
        "Ana is allergic to penicillin.",
        r#"[{"subject":"Ana","predicate":"allergic_to","object":"penicillin"}]"#,
    );
    let mut doc = export_doc(source.path());
    let valid_from = doc["facts"][0]["valid_from"].clone();
    doc["facts"][0]["valid_until"] = valid_from;
    doc["facts"][0]["close_reason"] = json!("stopped");
    doc["facts"][0]["close_authority"] = json!("heuristic");
    doc["facts"][0]["object"] = json!("aspirin");
    doc["facts"][0]["object_norm"] = json!("aspirin");

    let target = TempDir::new().unwrap();
    import_doc(target.path(), &doc, &source.path().join("forged-row.json"));

    let f = only_fact(target.path(), "Ana");
    assert!(f["valid_until"].is_null(), "{f}");
    assert_eq!(
        f["object"], "penicillin",
        "the journal said penicillin; the row section does not get a vote: {f}"
    );
}

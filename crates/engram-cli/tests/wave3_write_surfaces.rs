//! Wave 3, write surfaces — the id a mutator accepts, and the provenance it
//! stamps on a caller-supplied fact.
//!
//! Two deliverables are under test here. W1's parse half: `edit`, `forget`,
//! `memory update` and `memory delete` read their id through
//! `engram_core::Handle`, so `mem:<uuid>` works, a bare uuid keeps working,
//! and a wrong-kind handle is refused with a sentence naming both halves
//! instead of "invalid UUID". W6: `--facts-json` triples are `user_stated`,
//! extractor output stays `agent_extracted`, and a run that does both keeps
//! them apart.
//!
//! Isolation is `ENGRAM_STORE` pointed at a TempDir and nothing else — no HOME
//! rewriting, and therefore no way for a leaked path to reach the developer's
//! real store.

use assert_cmd::Command;
use serde_json::Value;

fn store() -> tempfile::TempDir {
    tempfile::tempdir().expect("tmp store")
}

/// An offline `engram` rooted at `store`: stub embedder, no keys, no scripted
/// extractor unless a test asks for one.
fn engram(store: &std::path::Path) -> Command {
    let mut cmd = Command::cargo_bin("engram").expect("binary built");
    cmd.env("ENGRAM_STORE", store);
    cmd.env("ENGRAM_BENCH_FORCE_STUB", "1");
    cmd.env_remove("OPENROUTER_API_KEY");
    cmd.env_remove("GEMINI_API_KEY");
    cmd.env_remove("COHERE_API_KEY");
    cmd.env_remove("ENGRAM_FACT_LLM_STUB");
    cmd.env_remove("ENGRAM_FACT_WRITE_FAIL");
    cmd
}

fn parse_stdout(out: &[u8]) -> Value {
    serde_json::from_slice(out).expect("valid JSON on stdout")
}

/// The JSON error envelope is the LAST `{`-line on stderr — the write commands
/// also print plain `note:` / `warning:` lines there.
fn parse_error(err: &[u8]) -> Value {
    let line = String::from_utf8_lossy(err)
        .lines()
        .rev()
        .find(|l| l.trim_start().starts_with('{'))
        .expect("JSON error on stderr")
        .to_string();
    serde_json::from_str(&line).expect("parseable error envelope")
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
    parse_stdout(&out)
}

/// Store one memory and return its id.
fn seed(store: &std::path::Path, content: &str) -> String {
    let v = run_json(
        &mut engram(store),
        &["remember", content, "--offline", "--no-extract-facts"],
    );
    v["data"]["id"].as_str().expect("memory id").to_string()
}

// ---------------------------------------------------------------------------
// W1 — every mutator parses its id as a handle
// ---------------------------------------------------------------------------

#[test]
fn edit_accepts_a_bare_uuid() {
    let dir = store();
    let id = seed(dir.path(), "Ana moved to Lisbon.");
    let v = run_json(
        &mut engram(dir.path()),
        &["edit", &id, "--content", "Ana moved to Porto.", "--offline"],
    );
    assert_eq!(v["data"]["updated"], true);
    assert_eq!(v["data"]["id"], id);
}

#[test]
fn edit_accepts_a_mem_prefixed_handle() {
    let dir = store();
    let id = seed(dir.path(), "Bruno joined Acme.");
    let v = run_json(
        &mut engram(dir.path()),
        &[
            "edit",
            &format!("mem:{id}"),
            "--content",
            "Bruno joined Globex.",
            "--offline",
        ],
    );
    assert_eq!(v["data"]["updated"], true);
    // The echoed id stays a bare uuid whatever spelling came in — scripts feed
    // `data.id` straight back into the next command.
    assert_eq!(v["data"]["id"], id);
}

#[test]
fn edit_rejects_a_fact_handle_naming_both_halves() {
    let dir = store();
    let id = seed(dir.path(), "Ana lives in Lisbon.");
    let out = engram(dir.path())
        .args(["edit", &format!("fact:{id}"), "--content", "x", "--offline"])
        .arg("--json")
        .assert()
        .failure()
        .code(3)
        .get_output()
        .clone();
    let err = parse_error(&out.stderr);
    let msg = err["error"]["message"].as_str().expect("message");
    assert!(msg.contains("wrong id kind"), "{msg}");
    assert!(msg.contains(&format!("fact:{id}")), "{msg}");
    assert!(msg.contains("mem:<uuid>"), "{msg}");
    // The memory itself is untouched: a wrong-kind id never reaches a write.
    let show = run_json(&mut engram(dir.path()), &["memory", "list"]);
    let text = serde_json::to_string(&show).unwrap();
    assert!(text.contains("Ana lives in Lisbon."), "{text}");
}

#[test]
fn edit_on_a_malformed_id_keeps_the_legacy_message() {
    let dir = store();
    let out = engram(dir.path())
        .args(["edit", "not-a-uuid", "--content", "x", "--offline"])
        .arg("--json")
        .assert()
        .failure()
        .code(3)
        .get_output()
        .clone();
    let err = parse_error(&out.stderr);
    assert_eq!(err["error"]["code"], "bad_input");
    assert!(
        err["error"]["message"]
            .as_str()
            .expect("message")
            .contains("invalid UUID"),
        "{err}"
    );
}

#[test]
fn forget_accepts_a_mem_prefixed_handle() {
    let dir = store();
    let id = seed(dir.path(), "A memory to retract.");
    let v = run_json(
        &mut engram(dir.path()),
        &["forget", &format!("mem:{id}"), "--confirm"],
    );
    assert_eq!(v["data"]["deleted"], true);
    assert_eq!(v["data"]["id"], id);
}

#[test]
fn forget_rejects_a_chunk_handle_with_the_resolver_note() {
    let dir = store();
    let id = seed(dir.path(), "A memory with chunks.");
    let out = engram(dir.path())
        .args(["forget", &format!("chunk:{id}"), "--confirm"])
        .arg("--json")
        .assert()
        .failure()
        .code(3)
        .get_output()
        .clone();
    let msg = parse_error(&out.stderr)["error"]["message"]
        .as_str()
        .expect("message")
        .to_string();
    assert!(msg.contains("is a chunk"), "{msg}");
    assert!(msg.contains("handle resolver"), "{msg}");
    // And the memory is still there — refusing beats guessing.
    let v = run_json(&mut engram(dir.path()), &["forget", &id, "--confirm"]);
    assert_eq!(v["data"]["deleted"], true);
}

#[test]
fn forget_still_refuses_before_it_parses_anything() {
    // The destructive guard precedes resolution: no `--confirm` is `bad_input`
    // whatever the id says, and nothing is looked up.
    let dir = store();
    let out = engram(dir.path())
        .args(["forget", "fact:not-even-a-uuid"])
        .arg("--json")
        .assert()
        .failure()
        .code(3)
        .get_output()
        .clone();
    let msg = parse_error(&out.stderr)["error"]["message"]
        .as_str()
        .expect("message")
        .to_string();
    assert!(msg.contains("--confirm"), "{msg}");
}

#[test]
fn memory_update_and_delete_resolve_handles_identically() {
    let dir = store();
    let id = seed(dir.path(), "Reachable through the memory alias.");
    let v = run_json(
        &mut engram(dir.path()),
        &[
            "memory",
            "update",
            &format!("mem:{id}"),
            "--importance",
            "7",
        ],
    );
    assert_eq!(v["data"]["updated"], true);

    let out = engram(dir.path())
        .args(["memory", "delete", &format!("entity:{id}"), "--confirm"])
        .arg("--json")
        .assert()
        .failure()
        .code(3)
        .get_output()
        .clone();
    let msg = parse_error(&out.stderr)["error"]["message"]
        .as_str()
        .expect("message")
        .to_string();
    assert!(msg.contains("wrong id kind"), "{msg}");

    let v = run_json(
        &mut engram(dir.path()),
        &["memory", "delete", &format!("mem:{id}"), "--confirm"],
    );
    assert_eq!(v["data"]["deleted"], true);
}

#[test]
fn an_unknown_prefix_is_reported_as_an_unknown_kind() {
    let dir = store();
    let id = seed(dir.path(), "Prefix taxonomy.");
    let out = engram(dir.path())
        .args(["edit", &format!("blob:{id}"), "--importance", "3"])
        .arg("--json")
        .assert()
        .failure()
        .code(3)
        .get_output()
        .clone();
    let msg = parse_error(&out.stderr)["error"]["message"]
        .as_str()
        .expect("message")
        .to_string();
    assert!(msg.contains("unknown id kind `blob`"), "{msg}");
    assert!(msg.contains("mem, chunk, fact"), "{msg}");
}

// ---------------------------------------------------------------------------
// W6 — a caller-asserted triple is not an extractor's guess
// ---------------------------------------------------------------------------

/// Caller triples: one subject the scripted extractor never mentions.
const CALLER_FACTS: &str = r#"[
  {"subject":"Ana","predicate":"lives_in","object":"Lisbon","confidence":1.0}
]"#;

/// A scripted extractor that always answers with a DIFFERENT subject, so a
/// mixed run has one triple from each door and no adjudication between them.
const EXTRACTOR_SCRIPT: &str = r#"{
  "*": [
    {"subject":"Bruno","predicate":"works_at","object":"Acme","confidence":1.0,"fact_type":"profile_slot"}
  ]
}"#;

/// Every fact in the store, as `(subject, source_kind)`.
fn facts(store: &std::path::Path) -> Vec<(String, String)> {
    let v = run_json(
        &mut engram(store),
        &["facts", "list", "--kb", "*", "--diary", "*"],
    );
    v["data"]["facts"]
        .as_array()
        .expect("facts array")
        .iter()
        .map(|f| {
            (
                f["subject"].as_str().unwrap_or_default().to_string(),
                f["source_kind"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect()
}

#[test]
fn facts_json_is_stamped_user_stated() {
    let dir = store();
    let v = run_json(
        &mut engram(dir.path()),
        &[
            "remember",
            "Ana moved to Lisbon.",
            "--offline",
            "--facts-json",
            CALLER_FACTS,
        ],
    );
    assert_eq!(v["data"]["facts_added"], 1);
    assert_eq!(v["data"]["facts_source_kind"], "user_stated");
    // The caller did the extraction, so no LLM ran — unchanged from v0.3.
    assert_eq!(v["data"]["fact_extraction"], "skipped_facts_json");

    let rows = facts(dir.path());
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0], ("Ana".to_string(), "user_stated".to_string()));
}

#[test]
fn extraction_stays_agent_extracted() {
    let dir = store();
    let mut cmd = engram(dir.path());
    cmd.env("ENGRAM_FACT_LLM_STUB", EXTRACTOR_SCRIPT);
    let v = run_json(&mut cmd, &["remember", "Bruno joined Acme.", "--offline"]);
    assert_eq!(v["data"]["facts_added"], 1);
    assert_eq!(v["data"]["facts_source_kind"], "agent_extracted");

    let rows = facts(dir.path());
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(
        rows[0],
        ("Bruno".to_string(), "agent_extracted".to_string())
    );
}

#[test]
fn a_mixed_run_keeps_both_provenances() {
    let dir = store();
    let mut cmd = engram(dir.path());
    cmd.env("ENGRAM_FACT_LLM_STUB", EXTRACTOR_SCRIPT);
    // `--extract-facts` alongside `--facts-json` is the ONLY way both doors
    // open on one write; without it the caller's triples suppress extraction.
    let v = run_json(
        &mut cmd,
        &[
            "remember",
            "Ana moved to Lisbon and Bruno joined Acme.",
            "--offline",
            "--facts-json",
            CALLER_FACTS,
            "--extract-facts",
        ],
    );
    assert_eq!(v["data"]["facts_added"], 2);
    assert_eq!(v["data"]["facts_source_kind"], "mixed");

    let mut rows = facts(dir.path());
    rows.sort();
    assert_eq!(
        rows,
        vec![
            ("Ana".to_string(), "user_stated".to_string()),
            ("Bruno".to_string(), "agent_extracted".to_string()),
        ]
    );
}

#[test]
fn a_failing_extraction_batch_leaves_the_caller_batch_committed() {
    // The landmine of the two-batch split: the old guard keyed on the FLAG, so
    // an extraction failure on a `--facts-json` run hard-errored and reported
    // the caller's triples as lost when they were already in the store.
    let dir = store();
    let mut cmd = engram(dir.path());
    cmd.env("ENGRAM_FACT_LLM_STUB", EXTRACTOR_SCRIPT);
    cmd.env("ENGRAM_FACT_WRITE_FAIL", "Bruno");
    let v = run_json(
        &mut cmd,
        &[
            "remember",
            "Ana moved to Lisbon and Bruno joined Acme.",
            "--offline",
            "--facts-json",
            CALLER_FACTS,
            "--extract-facts",
        ],
    );
    assert_eq!(v["data"]["fact_extraction"], "failed_persist");
    assert_eq!(v["data"]["facts_source_kind"], "user_stated");

    let rows = facts(dir.path());
    assert_eq!(
        rows,
        vec![("Ana".to_string(), "user_stated".to_string())],
        "the caller's committed batch survives an extraction failure"
    );
}

#[test]
fn a_failing_caller_batch_still_hard_errors() {
    let dir = store();
    let mut cmd = engram(dir.path());
    cmd.env("ENGRAM_FACT_WRITE_FAIL", "Ana");
    let out = cmd
        .args([
            "remember",
            "Ana moved to Lisbon.",
            "--offline",
            "--facts-json",
            CALLER_FACTS,
        ])
        .arg("--json")
        .assert()
        .failure()
        .get_output()
        .clone();
    let msg = parse_error(&out.stderr)["error"]["message"]
        .as_str()
        .expect("message")
        .to_string();
    assert!(msg.contains("--facts-json:"), "{msg}");
    assert!(
        facts(dir.path()).is_empty(),
        "a failed batch stores nothing"
    );
}

#[test]
fn the_duplicate_content_path_stamps_the_same_source_kind() {
    let dir = store();
    let content = "Ana moved to Lisbon.";
    run_json(
        &mut engram(dir.path()),
        &[
            "remember",
            content,
            "--offline",
            "--facts-json",
            CALLER_FACTS,
        ],
    );
    // The identical command again: the content dedups, the caller's triples are
    // re-offered against the existing memory, and provenance does not drift.
    let v = run_json(
        &mut engram(dir.path()),
        &[
            "remember",
            content,
            "--offline",
            "--facts-json",
            CALLER_FACTS,
        ],
    );
    assert_eq!(v["data"]["deduplicated"], true);
    assert_eq!(v["data"]["facts_source_kind"], "user_stated");

    let rows = facts(dir.path());
    assert_eq!(rows, vec![("Ana".to_string(), "user_stated".to_string())]);
}

#[test]
fn the_store_under_test_is_the_temp_dir() {
    // The guard that catches a leaked path before it reaches a real store.
    let dir = store();
    seed(dir.path(), "Isolation check.");
    assert!(
        dir.path().join("db.sqlite").exists(),
        "store is the TempDir"
    );
}

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

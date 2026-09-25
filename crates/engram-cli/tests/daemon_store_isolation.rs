//! Wave 4 — `--store` roots everything, and nothing escapes it.
//!
//! Brief constraint 1. The highest-risk path is `POST /v1/ingest`, which
//! re-enters the CLI as a child process: without the environment export a
//! `--store`-isolated daemon would ingest into the user's real store, which is
//! the one failure this flag exists to prevent.

mod daemon_support;

use daemon_support::*;
use serde_json::Value;

fn engram_bare() -> assert_cmd::Command {
    let mut cmd = assert_cmd::Command::cargo_bin("engram").expect("binary built");
    cmd.env_remove("ENGRAM_STORE")
        .env_remove("GEMINI_API_KEY")
        .env_remove("COHERE_API_KEY")
        .env_remove("OPENROUTER_API_KEY")
        .env("ENGRAM_BENCH_FORCE_STUB", "1");
    cmd
}

#[test]
fn the_flag_roots_everything() {
    let tmp = tempfile::tempdir().expect("tmp");
    let out = engram_bare()
        .args([
            "--store",
            tmp.path().to_str().unwrap(),
            "remember",
            "Isolated memory.",
            "--json",
        ])
        .output()
        .expect("engram runs");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        tmp.path().join("db.sqlite").exists(),
        "the write must land in the flagged store"
    );
}

#[test]
fn the_flag_beats_the_environment() {
    let env_store = tempfile::tempdir().expect("tmp");
    let flag_store = tempfile::tempdir().expect("tmp");
    let out = engram_bare()
        .env("ENGRAM_STORE", env_store.path())
        .args([
            "--store",
            flag_store.path().to_str().unwrap(),
            "remember",
            "Precedence matters.",
            "--json",
        ])
        .output()
        .expect("engram runs");
    assert!(out.status.success());
    assert!(
        flag_store.path().join("db.sqlite").exists(),
        "interfaces §5: --store > ENGRAM_STORE > ProjectDirs"
    );
    assert!(
        !env_store.path().join("db.sqlite").exists(),
        "the losing store must be untouched"
    );
}

#[test]
fn agent_info_with_a_store_flag_opens_and_creates_nothing() {
    let tmp = tempfile::tempdir().expect("tmp");
    let target = tmp.path().join("never-created");
    let out = engram_bare()
        .args(["--store", target.to_str().unwrap(), "agent-info"])
        .output()
        .expect("engram runs");
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains(target.to_str().unwrap()),
        "the manifest must report the flagged paths"
    );
    assert!(
        !target.exists(),
        "a discovery command that materialises a store directory is the same \
         class of bug as one that migrates a database"
    );
}

#[test]
fn an_empty_store_flag_is_refused_and_never_read_as_unset() {
    // clap refuses it first, by name. `apply_store_override` refuses it too,
    // for library callers that never see clap — what must not happen is the
    // empty string reaching `paths::`, where an empty `ENGRAM_STORE` means
    // "unset" and the command would silently touch the real store.
    let out = engram_bare()
        .args(["--store", "", "recall", "anything", "--json"])
        .output()
        .expect("engram runs");
    assert!(
        !out.status.success(),
        "an empty value is not the same as leaving it unset"
    );
    let text = String::from_utf8_lossy(&out.stderr);
    assert!(
        text.contains("--store"),
        "the refusal names the flag: {text}"
    );
}

#[test]
fn a_relative_store_flag_is_made_absolute() {
    // Child processes inherit `ENGRAM_STORE`, and a relative path means
    // something different in a child with a different working directory.
    let tmp = tempfile::tempdir().expect("tmp");
    let out = engram_bare()
        .current_dir(tmp.path())
        .args(["--store", "scratch", "remember", "Relative.", "--json"])
        .output()
        .expect("engram runs");
    assert!(out.status.success());
    assert!(tmp.path().join("scratch").join("db.sqlite").exists());
}

#[test]
fn the_ingest_subprocess_inherits_the_isolated_store() {
    // Brief constraint 1's highest-risk path, end to end.
    let store = Store::new();
    let daemon = Daemon::start(&store);
    let doc = store.path().join("note.md");
    std::fs::write(&doc, "# Isolation\n\nA document ingested over HTTP.\n").expect("write doc");

    let (status, body) = daemon.post(
        "/v1/ingest",
        &serde_json::json!({ "path": doc.to_str().unwrap(), "kb": "default", "offline": true })
            .to_string(),
    );
    assert_eq!(status, 200, "{body}");
    let docs = store.seed(&["documents", "list", "--kb", "default"]);
    let names = docs["data"]["documents"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        !names.is_empty(),
        "the ingested document must be in the ISOLATED store: {docs}"
    );
}

#[test]
fn the_descriptor_is_written_while_serving_and_a_dead_one_is_ignored() {
    let store = Store::new();
    let descriptor = store.path().join("engramd.json");
    {
        let daemon = Daemon::start(&store);
        assert!(
            descriptor.exists(),
            "the descriptor is how the CLI knows whether a scheduler exists"
        );
        let d: Value = serde_json::from_slice(&std::fs::read(&descriptor).unwrap()).unwrap();
        assert_eq!(d["port"], daemon.port);
        assert_eq!(d["token_required"], false);
    }
    // The daemon was killed, so the descriptor is stale. Any command that
    // consults it must delete it rather than wait forever for a scheduler that
    // does not exist.
    let submitted = store.seed(&["jobs", "submit", "--kind", "compile", "--kb", "default"]);
    assert!(
        submitted["data"]["job"]["status"].is_string(),
        "a stale descriptor must not make submit hang: {submitted}"
    );
    assert!(
        !descriptor.exists(),
        "the first caller that notices a dead daemon deletes its descriptor"
    );
}

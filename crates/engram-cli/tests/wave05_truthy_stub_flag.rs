//! Wave 0.5 — `ENGRAM_BENCH_FORCE_STUB` means what the manifest says it means.
//!
//! `agent_info` promises: "The sole override is the test hook
//! ENGRAM_BENCH_FORCE_STUB with a truthy value (1/true); 0, false or empty
//! leaves cloud semantics intact on both sides." Wave 0 made that true for
//! `remember`, `edit` and the read path via `config::stub_forced()`. Six other
//! surfaces still asked `std::env::var(..).is_ok()`, which is true for
//! `ENGRAM_BENCH_FORCE_STUB=0` — so the manifest was lying about them.
//!
//! `reindex` is the one that costs a corpus: a shell exporting the flag as `0`
//! (the natural way to say "off") made `engram reindex --all` rewrite EVERY
//! chunk to 64-dim stub vectors and report success, while the key it was meant
//! to migrate onto sat right there in the environment. That is precisely the
//! command the mixed-embedding error tells users to run, so the remedy was the
//! poison.
//!
//! Driven over an empty store so `embed_batch` short-circuits before any HTTP
//! call (`gemini.rs`: `if texts.is_empty() { return Ok(Vec::new()) }`): the key
//! below is never dialled, only believed.

use assert_cmd::Command;
use serde_json::Value;
use std::path::Path;

fn point_at_home(cmd: &mut Command, home: &Path) {
    // A developer's exported ENGRAM_STORE outranks HOME/XDG (cli.rs documents
    // the precedence), so without this line the temp-HOME isolation below is
    // bypassed and mutator tests run against the REAL store.
    cmd.env_remove("ENGRAM_STORE");
    cmd.env("HOME", home);
    cmd.env("XDG_CONFIG_HOME", home.join(".config"));
    cmd.env("XDG_DATA_HOME", home.join(".local/share"));
    cmd.env("XDG_CACHE_HOME", home.join(".cache"));
}

fn base_cmd(home: &Path) -> Command {
    let mut cmd = Command::cargo_bin("engram").expect("binary built");
    cmd.env_remove("ENGRAM_BENCH_FORCE_STUB");
    cmd.env_remove("GEMINI_API_KEY");
    cmd.env_remove("COHERE_API_KEY");
    cmd.env_remove("OPENROUTER_API_KEY");
    cmd.env_remove("ENGRAM_EMBED_FAIL");
    point_at_home(&mut cmd, home);
    cmd
}

fn reindex_model(home: &Path, flag: Option<&str>) -> Value {
    let mut cmd = base_cmd(home);
    // A real-looking key that is never dialled: the store is empty, so the
    // embedder is asked what it IS and never asked to embed anything.
    cmd.env("GEMINI_API_KEY", "AIza-not-a-real-key-never-dialled");
    if let Some(v) = flag {
        cmd.env("ENGRAM_BENCH_FORCE_STUB", v);
    }
    let out = cmd
        .args(["reindex", "--all", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    serde_json::from_slice(&out).expect("valid JSON on stdout")
}

#[test]
fn a_falsy_stub_flag_does_not_rewrite_an_index_to_stub_vectors() {
    let home = tempfile::tempdir().expect("tempdir");
    for falsy in ["0", "false", "FALSE", "", "  "] {
        let v = reindex_model(home.path(), Some(falsy));
        assert_eq!(
            v["data"]["model"], "gemini-embedding-2",
            "ENGRAM_BENCH_FORCE_STUB={falsy:?} must leave cloud semantics intact, got {v}"
        );
        assert_eq!(v["data"]["dimensions"], 1536, "flag {falsy:?}");
    }
}

#[test]
fn a_truthy_stub_flag_still_forces_the_stub_for_reindex() {
    let home = tempfile::tempdir().expect("tempdir");
    for truthy in ["1", "true", "TRUE", "yes"] {
        let v = reindex_model(home.path(), Some(truthy));
        assert_eq!(
            v["data"]["model"], "stub-64",
            "ENGRAM_BENCH_FORCE_STUB={truthy:?} must force the stub, got {v}"
        );
        assert_eq!(v["data"]["dimensions"], 64, "flag {truthy:?}");
    }
}

#[test]
fn an_unset_stub_flag_leaves_reindex_on_the_configured_key() {
    let home = tempfile::tempdir().expect("tempdir");
    let v = reindex_model(home.path(), None);
    assert_eq!(v["data"]["model"], "gemini-embedding-2");
    assert_eq!(v["data"]["dimensions"], 1536);
}

//! Wave 0.5 — one offline policy for writes.
//!
//! Every READ surface refuses a cloud profile with no Gemini key; the write
//! paths silently embedded with the 64-dim stub instead. The store that
//! produced was one ordinary recall then REFUSED to serve ("embedding metadata
//! mismatch", which reads as a corrupt index rather than a missing key), and
//! configuring a key later left it half stub- and half cloud-embedded — a mix
//! nothing but a full `engram reindex` repairs. Worse than unsearchable:
//! `novelty_gate` skips candidate vectors of a different width, so fact dedup
//! silently stops working too.
//!
//! These tests drive the built binary against an isolated HOME with no keys
//! reachable, exactly as a keyless user's shell would.

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

/// A keyless user's shell: no bench hook, no provider keys, however the
/// developer's own environment happens to be configured.
fn cloud_cmd_in(home: &Path) -> Command {
    let mut cmd = Command::cargo_bin("engram").expect("binary built");
    cmd.env_remove("ENGRAM_BENCH_FORCE_STUB");
    cmd.env_remove("GEMINI_API_KEY");
    cmd.env_remove("COHERE_API_KEY");
    cmd.env_remove("OPENROUTER_API_KEY");
    cmd.env_remove("TYPESAFE_API_KEY");
    cmd.env_remove("GEMINI_API_KEY_FALLBACKS");
    cmd.env_remove("ENGRAM_JUDGE_STUB");
    cmd.env_remove("ENGRAM_JUDGE_GROUNDING");
    cmd.env_remove("ENGRAM_JUDGE_ADJUDICATE");
    cmd.env_remove("ENGRAM_JUDGE_DEDUPE");
    cmd.env_remove("ENGRAM_JUDGE_RELEVANCE");
    cmd.env_remove("ENGRAM_JUDGE_LOG");
    cmd.env_remove("ENGRAM_EMBED_FAIL");
    point_at_home(&mut cmd, home);
    cmd
}

/// The ordinary CI shape: the bench hook forces the stub with no flag at all.
fn stub_cmd_in(home: &Path) -> Command {
    let mut cmd = cloud_cmd_in(home);
    cmd.env("ENGRAM_BENCH_FORCE_STUB", "1");
    cmd
}

fn parse_stdout(out: &[u8]) -> Value {
    serde_json::from_slice(out).expect("valid JSON on stdout")
}

/// The JSON error envelope is the LAST `{`-line on stderr — writes also emit
/// plain `note:` / `warning:` lines there.
fn stderr_json(err: &[u8]) -> Value {
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

/// Both remedies, in the words the user has to type.
fn assert_names_both_remedies(v: &Value) {
    let msg = v["error"]["message"].as_str().expect("message");
    assert_eq!(v["error"]["code"], "config_error", "{v}");
    assert!(msg.contains("GEMINI_API_KEY"), "{msg}");
    assert!(msg.contains("keys.gemini"), "{msg}");
    assert!(msg.contains("--offline"), "{msg}");
}

fn home() -> tempfile::TempDir {
    tempfile::tempdir().expect("tmp dir")
}

const NOTE: &str = "Ana keeps her sailing logs in a notebook by the door.";
const REWRITE: &str = "Ana keeps her sailing logs in a shared spreadsheet now.";
const FAKE_KEY: &str = "fake-key-never-used-the-guard-fires-first";

/// Seed one memory offline and return its id.
fn seed(home: &Path) -> String {
    let v = run_json(
        &mut stub_cmd_in(home),
        &["remember", NOTE, "--no-extract-facts"],
    );
    v["data"]["id"].as_str().expect("a memory id").to_string()
}

fn export(home: &Path) -> Value {
    run_json(&mut stub_cmd_in(home), &["export", "--format", "json"])
}

/// THE DEFECT on the `remember` side.
#[test]
fn a_keyless_remember_refuses_instead_of_storing_stub_vectors() {
    let tmp = home();
    let assert = cloud_cmd_in(tmp.path())
        .args(["remember", NOTE, "--no-extract-facts", "--json"])
        .assert()
        .failure()
        .code(2);
    assert_names_both_remedies(&stderr_json(&assert.get_output().stderr));

    // The refusal happens before anything is read or written.
    let dump = export(tmp.path());
    assert_eq!(dump["data"]["counts"]["memories"], 0, "{dump}");
    assert_eq!(dump["data"]["counts"]["chunks"], 0, "{dump}");
}

/// The deliberate stub still works end to end.
#[test]
fn an_offline_remember_stores_and_offline_recall_finds_it() {
    let tmp = home();
    let v = run_json(
        &mut cloud_cmd_in(tmp.path()),
        &["remember", NOTE, "--no-extract-facts", "--offline"],
    );
    assert_eq!(v["data"]["stored"], true, "{v}");
    assert_eq!(v["metadata"]["embedder"], "stub-64", "{v}");

    let found = run_json(
        &mut cloud_cmd_in(tmp.path()),
        &["recall", "sailing logs notebook", "--profile", "offline"],
    );
    assert!(
        found["data"]["results"]
            .as_array()
            .map(|r| !r.is_empty())
            .unwrap_or(false),
        "{found}"
    );
    assert_eq!(found["metadata"]["embed_model"], "stub-64", "{found}");
}

/// An explicit opt-in is an instruction, not a hint — and it never bills.
#[test]
fn an_offline_write_spends_nothing_even_with_a_key_configured() {
    let tmp = home();
    let v = run_json(
        cloud_cmd_in(tmp.path()).env("GEMINI_API_KEY", FAKE_KEY),
        &["remember", NOTE, "--no-extract-facts", "--offline"],
    );
    assert_eq!(v["data"]["stored"], true, "{v}");
    assert_eq!(v["metadata"]["embedder"], "stub-64", "{v}");
    let usage = run_json(&mut stub_cmd_in(tmp.path()), &["usage"]);
    assert_eq!(usage["data"]["totals"]["events"], 0, "{usage}");
}

/// `edit`'s teardown is destructive, so "intact" is the load-bearing half.
#[test]
fn a_keyless_edit_refuses_and_leaves_the_memory_intact() {
    let tmp = home();
    let id = seed(tmp.path());
    let assert = cloud_cmd_in(tmp.path())
        .args(["edit", &id, "--content", REWRITE, "--json"])
        .assert()
        .failure()
        .code(2);
    assert_names_both_remedies(&stderr_json(&assert.get_output().stderr));

    let dump = export(tmp.path());
    assert_eq!(dump["data"]["chunks"][0]["content"], NOTE, "{dump}");
}

#[test]
fn an_offline_edit_replaces_the_chunks() {
    let tmp = home();
    let id = seed(tmp.path());
    let v = run_json(
        &mut cloud_cmd_in(tmp.path()),
        &["edit", &id, "--content", REWRITE, "--offline"],
    );
    assert_eq!(v["data"]["updated"], true, "{v}");
    assert!(
        v["data"]["chunks_replaced"].as_u64().unwrap_or(0) >= 1,
        "{v}"
    );
    let dump = export(tmp.path());
    assert_eq!(dump["data"]["chunks"][0]["content"], REWRITE, "{dump}");
}

/// The boundary a naive top-of-function gate breaks: an importance-only edit
/// re-embeds nothing, so it needs no key.
#[test]
fn an_importance_only_edit_needs_no_embedder() {
    let tmp = home();
    let id = seed(tmp.path());
    let v = run_json(
        &mut cloud_cmd_in(tmp.path()),
        &["edit", &id, "--importance", "9"],
    );
    assert_eq!(v["data"]["updated"], true, "{v}");
}

/// Error precedence: a bad argument is still exit 3, never masked by the
/// environment.
#[test]
fn bad_arguments_still_beat_the_missing_key() {
    let tmp = home();
    let id = seed(tmp.path());
    let empty = cloud_cmd_in(tmp.path())
        .args(["remember", "", "--json"])
        .assert()
        .failure()
        .code(3);
    assert_eq!(
        stderr_json(&empty.get_output().stderr)["error"]["code"],
        "bad_input"
    );

    let blank_edit = cloud_cmd_in(tmp.path())
        .args(["edit", &id, "--content", "", "--json"])
        .assert()
        .failure()
        .code(3);
    let err = stderr_json(&blank_edit.get_output().stderr);
    assert_eq!(err["error"]["code"], "bad_input", "{err}");
    assert!(
        err["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("cannot be empty"),
        "{err}"
    );
}

/// Flag parity between the two commands and their aliases is a documented
/// promise (`memory_add_has_full_flag_parity_with_remember`).
#[test]
fn the_memory_aliases_take_the_same_flag() {
    let tmp = home();
    let added = run_json(
        &mut cloud_cmd_in(tmp.path()),
        &["memory", "add", NOTE, "--no-extract-facts", "--offline"],
    );
    let id = added["data"]["id"]
        .as_str()
        .expect("a memory id")
        .to_string();
    let updated = run_json(
        &mut cloud_cmd_in(tmp.path()),
        &["memory", "update", &id, "--content", REWRITE, "--offline"],
    );
    assert_eq!(updated["data"]["updated"], true, "{updated}");

    // ...and both refuse without it.
    cloud_cmd_in(tmp.path())
        .args(["memory", "add", "A second keyless note.", "--json"])
        .assert()
        .failure()
        .code(2);
    cloud_cmd_in(tmp.path())
        .args(["memory", "update", &id, "--content", NOTE, "--json"])
        .assert()
        .failure()
        .code(2);
}

/// The invariant every existing suite runs on, plus the Wave 0 truthiness rule
/// on the write side: `=0`, `false` and empty leave cloud semantics intact.
#[test]
fn the_bench_stub_hook_still_writes_and_a_falsy_value_does_not() {
    let tmp = home();
    let v = run_json(
        &mut stub_cmd_in(tmp.path()),
        &["remember", NOTE, "--no-extract-facts"],
    );
    assert_eq!(v["data"]["stored"], true, "{v}");

    for falsy in ["0", "false", ""] {
        let assert = cloud_cmd_in(tmp.path())
            .env("ENGRAM_BENCH_FORCE_STUB", falsy)
            .args([
                "remember",
                "A note written under a falsy stub flag.",
                "--no-extract-facts",
                "--json",
            ])
            .assert()
            .failure()
            .code(2);
        assert_names_both_remedies(&stderr_json(&assert.get_output().stderr));
    }
}

/// The guard must not fire when a key DOES exist. `ENGRAM_EMBED_FAIL` proves
/// the call reached the embed step without dialling anything.
#[test]
fn a_configured_key_reaches_the_embedder_instead_of_the_guard() {
    let tmp = home();
    let assert = cloud_cmd_in(tmp.path())
        .env("GEMINI_API_KEY", FAKE_KEY)
        .env("ENGRAM_EMBED_FAIL", "simulated 429")
        .args(["remember", NOTE, "--no-extract-facts", "--json"])
        .assert()
        .failure()
        .code(1);
    assert_eq!(
        stderr_json(&assert.get_output().stderr)["error"]["code"],
        "transient_error"
    );
}

/// Release-gate finding (Codex): the budget guard fired before the embedder
/// was resolved, so a ZERO spend cap blocked `remember --offline` — a write
/// that spends nothing failing with a budget error is a failure mode with no
/// money behind it. The cap now gates paid calls only.
#[test]
fn a_zero_spend_cap_does_not_block_a_free_offline_write() {
    let tmp = home();
    run_json(
        &mut cloud_cmd_in(tmp.path()),
        &["budget", "set", "--kb", "capped", "--daily-usd", "0"],
    );
    let v = run_json(
        &mut cloud_cmd_in(tmp.path()),
        &[
            "remember",
            "A free offline note under a zero cap.",
            "--kb",
            "capped",
            "--offline",
            "--no-extract-facts",
        ],
    );
    assert_eq!(v["data"]["stored"], true, "{v}");
}

/// Same rule on `edit`: an --offline re-embed spends nothing, so the cap
/// cannot block it.
#[test]
fn a_zero_spend_cap_does_not_block_a_free_offline_edit() {
    let tmp = home();
    let v = run_json(
        &mut cloud_cmd_in(tmp.path()),
        &[
            "remember",
            "Seed for the capped offline edit.",
            "--kb",
            "capped",
            "--offline",
            "--no-extract-facts",
        ],
    );
    let id = v["data"]["id"].as_str().expect("id").to_string();
    run_json(
        &mut cloud_cmd_in(tmp.path()),
        &["budget", "set", "--kb", "capped", "--daily-usd", "0"],
    );
    let e = run_json(
        &mut cloud_cmd_in(tmp.path()),
        &[
            "edit",
            id.as_str(),
            "--content",
            "Rewritten under a zero cap, still free.",
            "--offline",
        ],
    );
    assert_eq!(e["data"]["updated"], true, "{e}");
}

/// Release-gate finding (Codex): keyless `reindex` silently REPLACED a cloud
/// index with 64-dim stub vectors and reported success — the recovery path
/// the mixed-embedding error names, destroying what it was meant to repair.
#[test]
fn keyless_reindex_refuses_instead_of_rewriting_the_index() {
    let tmp = home();
    run_json(
        &mut cloud_cmd_in(tmp.path()),
        &[
            "remember",
            "A stub-embedded note the reindex must not silently rewrite.",
            "--kb",
            "personal",
            "--offline",
            "--no-extract-facts",
        ],
    );
    let assert = cloud_cmd_in(tmp.path())
        .args(["reindex", "--all", "--json"])
        .assert()
        .failure()
        .code(2);
    let err = stderr_json(&assert.get_output().stderr);
    let msg = err["error"]["message"].as_str().unwrap_or_default();
    assert!(msg.contains("--offline"), "must name the remedy: {msg}");

    // The explicit opt-in works, and says what it wrote.
    let v = run_json(
        &mut cloud_cmd_in(tmp.path()),
        &["reindex", "--all", "--offline"],
    );
    assert_eq!(v["data"]["model"], "stub-64", "{v}");
}

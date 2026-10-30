//! Wave 0.5 — a paid embedding is billed even when the call that made it fails,
//! and `edit` stops paying for work it had no business ordering.
//!
//! Two defects, one ledger. A Gemini `embed_batch` is SEVERAL billed HTTP
//! requests: if request 1 was served and request 2 failed, the whole call
//! returned `Err` and NOTHING was recorded — provider spend with no ledger row,
//! and every `engram budget` cap silently losing ground. And `edit` had no
//! budget check at all, made its paid call before discovering the uuid named no
//! memory, attributed its usage row to no kb/diary, and recorded that row AFTER
//! its transaction with `?`, so a bookkeeping failure exited non-zero on an edit
//! that had already committed.
//!
//! HONEST LIMITATION: the multi-batch NETWORK failure cannot be driven end to
//! end here — `gemini::ENDPOINT` is a hardcoded const with no base-URL override,
//! and adding one is a larger change than this item warrants. The accounting is
//! proved in two halves: `plan_batches` plus the charged-prefix invariants as
//! unit tests in `engram-embed`, and the ledger/ordering behaviour here.

use assert_cmd::Command;
use serde_json::Value;

fn point_at_home(cmd: &mut Command, home: &std::path::Path) {
    // A developer's exported ENGRAM_STORE outranks HOME/XDG (cli.rs documents
    // the precedence), so without this line the temp-HOME isolation below is
    // bypassed and mutator tests run against the REAL store.
    cmd.env_remove("ENGRAM_STORE");
    cmd.env_remove("GEMINI_API_KEY");
    cmd.env_remove("COHERE_API_KEY");
    cmd.env_remove("OPENROUTER_API_KEY");
    cmd.env_remove("ANTHROPIC_API_KEY");
    cmd.env_remove("TYPESAFE_API_KEY");
    cmd.env_remove("GEMINI_API_KEY_FALLBACKS");
    cmd.env_remove("ENGRAM_JUDGE_STUB");
    cmd.env_remove("ENGRAM_JUDGE_GROUNDING");
    cmd.env_remove("ENGRAM_JUDGE_ADJUDICATE");
    cmd.env_remove("ENGRAM_JUDGE_DEDUPE");
    cmd.env_remove("ENGRAM_JUDGE_RELEVANCE");
    cmd.env_remove("ENGRAM_JUDGE_LOG");
    cmd.env_remove("ENGRAM_EMBED_FAIL");
    cmd.env("HOME", home);
    cmd.env("XDG_CONFIG_HOME", home.join(".config"));
    cmd.env("XDG_DATA_HOME", home.join(".local/share"));
    cmd.env("XDG_CACHE_HOME", home.join(".cache"));
}

/// Offline `engram` against an isolated home (stub embedder, no API keys).
fn engram(home: &std::path::Path) -> Command {
    let mut cmd = Command::cargo_bin("engram").expect("binary built");
    cmd.env("ENGRAM_BENCH_FORCE_STUB", "1");
    point_at_home(&mut cmd, home);
    cmd
}

/// The PAID shape: a key resolves and the bench hook is gone, so any gate that
/// fails to fire would reach a real provider — `ENGRAM_EMBED_FAIL` is the
/// tripwire that catches it without dialling anything.
fn cloud_cmd(home: &std::path::Path) -> Command {
    let mut cmd = Command::cargo_bin("engram").expect("binary built");
    point_at_home(&mut cmd, home);
    cmd.env_remove("ENGRAM_BENCH_FORCE_STUB");
    cmd.env("GEMINI_API_KEY", UNUSED_KEY);
    cmd
}

fn parse_stdout(out: &[u8]) -> Value {
    serde_json::from_slice(out).expect("valid JSON on stdout")
}

/// The JSON error envelope is the LAST `{`-line on stderr.
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

fn seed(home: &std::path::Path, kb: &str, content: &str) -> String {
    let v = run_json(
        &mut engram(home),
        &["remember", content, "--kb", kb, "--no-extract-facts"],
    );
    v["data"]["id"].as_str().expect("a memory id").to_string()
}

fn export(home: &std::path::Path) -> Value {
    run_json(&mut engram(home), &["export", "--format", "json"])
}

fn usage(home: &std::path::Path) -> Value {
    run_json(&mut engram(home), &["usage"])
}

const UNUSED_KEY: &str = "fake-key-never-used-the-guard-fires-first";
const FAULT: &str = "simulated 429 from the embedder";
const ABSENT: &str = "3f2a1c64-0000-4000-8000-0000000000ff";
const ORIGINAL: &str = "The original note about the regatta in Cascais.";
const REWRITE: &str =
    "A longer rewrite of the note, with enough prose to chunk into something real.\n\n\
     The second paragraph keeps the memory well above the chunker's floor so the \
     replacement is a genuine swap rather than a no-op.";

fn home() -> tempfile::TempDir {
    tempfile::tempdir().expect("tmp dir")
}

/// THE DEFECT. The uuid parses, so `edit` used to chunk, embed (paid!) and only
/// then discover inside the transaction that no such memory exists.
#[test]
fn an_unknown_uuid_is_rejected_before_the_embedder_is_dialled() {
    let tmp = home();
    seed(tmp.path(), "personal", ORIGINAL);
    let assert = engram(tmp.path())
        // The injected fault sits exactly where the paid call is: if the
        // existence check ran late, the fault would win instead.
        .env("ENGRAM_EMBED_FAIL", FAULT)
        .args(["edit", ABSENT, "--content", REWRITE, "--json"])
        .assert()
        .failure()
        .code(3);
    let err = parse_error(&assert.get_output().stderr);
    // Same exit code, honest code string: wave 3 replaced the guessed
    // "no memory found for id … (already deleted?)" with `not_found`, so an
    // agent can branch on the kind of failure instead of reading prose.
    assert_eq!(err["error"]["code"], "not_found", "{err}");
    assert!(
        err["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains(ABSENT),
        "{err}"
    );
}

/// The consequence: the caller was billed for editing a memory that does not
/// exist, and the store still says nothing changed.
#[test]
fn an_edit_of_an_unknown_uuid_records_no_usage_event() {
    let tmp = home();
    seed(tmp.path(), "personal", ORIGINAL);
    cloud_cmd(tmp.path())
        .env("ENGRAM_EMBED_FAIL", FAULT)
        .args(["edit", ABSENT, "--content", REWRITE, "--json"])
        .assert()
        .failure()
        .code(3);

    assert_eq!(
        usage(tmp.path())["data"]["totals"]["events"],
        0,
        "an edit of a nonexistent memory reached the embedder"
    );
    let dump = export(tmp.path());
    assert_eq!(dump["data"]["chunks"][0]["content"], ORIGINAL, "{dump}");
}

/// A CLOUD `edit --content` re-embeds, so it spends — and every other paid
/// surface stops at the cap. `edit` had no `enforce_budget` call at all.
/// The cap gates paid calls only, so the cloud shape here carries a key:
/// under the stub an edit is free and the cap deliberately does not fire
/// (see wave05_offline_writes::a_zero_spend_cap_does_not_block_a_free_offline_edit).
#[test]
fn an_edit_under_a_zero_budget_is_refused_before_paying() {
    let tmp = home();
    // Seed BEFORE the cap: `remember` is budget-gated too.
    let id = seed(tmp.path(), "capped", ORIGINAL);
    run_json(
        &mut engram(tmp.path()),
        &["budget", "set", "--kb", "capped", "--daily-usd", "0"],
    );

    let assert = engram(tmp.path())
        .env_remove("ENGRAM_BENCH_FORCE_STUB")
        .env("GEMINI_API_KEY", "fake-key-the-cap-fires-first")
        .env("ENGRAM_EMBED_FAIL", FAULT)
        .args(["edit", &id, "--content", REWRITE, "--json"])
        .assert()
        .failure()
        .code(4);
    let err = parse_error(&assert.get_output().stderr);
    assert_eq!(err["error"]["code"], "rate_limited", "{err}");

    let dump = export(tmp.path());
    assert_eq!(
        dump["data"]["chunks"][0]["content"], ORIGINAL,
        "a refused edit must leave the memory alone"
    );
}

/// Both "no such memory" branches must answer with the same exit code — the
/// reorder is what made them agree, and this is the guard against them
/// drifting apart again.
#[test]
fn an_importance_only_edit_of_an_unknown_uuid_agrees_on_the_same_exit_code() {
    let tmp = home();
    seed(tmp.path(), "personal", ORIGINAL);
    let assert = engram(tmp.path())
        .args(["edit", ABSENT, "--importance", "3", "--json"])
        .assert()
        .failure()
        .code(3);
    assert_eq!(
        parse_error(&assert.get_output().stderr)["error"]["code"],
        "not_found"
    );
}

/// The deliberate decision: a free metadata update is not gated on a spend cap,
/// because refusing it would be a new failure mode with no money behind it.
#[test]
fn an_importance_only_edit_is_not_blocked_by_a_spend_cap() {
    let tmp = home();
    let id = seed(tmp.path(), "capped", ORIGINAL);
    run_json(
        &mut engram(tmp.path()),
        &["budget", "set", "--kb", "capped", "--daily-usd", "0"],
    );
    let v = run_json(&mut engram(tmp.path()), &["edit", &id, "--importance", "9"]);
    assert_eq!(v["data"]["updated"], true, "{v}");
    assert_eq!(usage(tmp.path())["data"]["totals"]["events"], 0);
}

/// Regression guard for the whole reorder: the ordinary offline edit still
/// swaps its chunks, still vectors them, and still bills nothing.
#[test]
fn a_successful_offline_edit_still_replaces_its_chunks_and_bills_nothing() {
    let tmp = home();
    let id = seed(tmp.path(), "personal", ORIGINAL);
    let v = run_json(
        &mut engram(tmp.path()),
        &["edit", &id, "--content", REWRITE],
    );
    assert_eq!(v["data"]["updated"], true, "{v}");
    assert!(
        v["data"]["chunks_replaced"].as_u64().unwrap_or(0) >= 1,
        "{v}"
    );

    let dump = export(tmp.path());
    let chunks = dump["data"]["chunks"].as_array().expect("chunks");
    assert!(!chunks.is_empty());
    assert!(
        chunks.iter().all(|c| c["embedding_b64"].is_string()),
        "a replaced chunk with no vector can never be returned by dense recall: {dump}"
    );
    assert!(
        chunks.iter().any(|c| c["content"]
            .as_str()
            .unwrap_or_default()
            .contains("rewrite")),
        "{dump}"
    );
    assert_eq!(
        usage(tmp.path())["data"]["totals"]["events"],
        0,
        "the offline stub is free and must never be billed"
    );
}

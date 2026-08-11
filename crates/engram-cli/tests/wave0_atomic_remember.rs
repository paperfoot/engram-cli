//! Wave 0.1 — `remember` is atomic: no committed memory without its chunks.
//!
//! `remember` used to insert the memory row, THEN chunk, THEN make a network
//! embed call, THEN insert the chunks. A failed embed left a committed memory
//! with no chunks: dense recall can never return it, and the exact-content
//! dedup at the top of `remember` turns every re-save into a successful-looking
//! no-op — the content is unreachable forever while the CLI reports success.
//! These tests drive the failure through `ENGRAM_EMBED_FAIL` and assert the
//! store is untouched.

use assert_cmd::Command;
use serde_json::Value;

/// Point a spawned binary's data/config/cache at `home` so no test can read or
/// write the developer's real memory store.
fn point_at_home(cmd: &mut Command, home: &std::path::Path) {
    // A developer's exported ENGRAM_STORE outranks HOME/XDG (cli.rs documents
    // the precedence), so without this line the temp-HOME isolation below is
    // bypassed and mutator tests run against the REAL store.
    cmd.env_remove("ENGRAM_STORE");
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

fn parse_stdout(out: &[u8]) -> Value {
    serde_json::from_slice(out).expect("valid JSON on stdout")
}

/// The JSON error envelope is the LAST `{`-line on stderr — `remember` also
/// writes plain `note:`/`warning:` lines there.
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

fn export(home: &std::path::Path) -> Value {
    run_json(&mut engram(home), &["export", "--format", "json"])
}

fn export_kb(home: &std::path::Path, kb: &str) -> Value {
    run_json(
        &mut engram(home),
        &["export", "--format", "json", "--kb", kb],
    )
}

const KB: &str = "wave0-atomic";
const FAULT: &str = "simulated 429 from the embedder";

/// A doc that produces several chunks, so the guarantee is tested on a set.
fn multi_chunk_note() -> String {
    let mut doc = String::new();
    for day in 16..24 {
        doc.push_str(&format!("Sep {day}, 2025\n\n"));
        doc.push_str(&format!(
            "Met Noemi at the Belgrade summit on day {day} and she mentioned her birthday is \
             on the 3rd of April, which is worth remembering because she throws a party every \
             year at the family house outside the city.\n\n"
        ));
    }
    doc
}

/// The core defect: an embed that never returned must leave nothing behind —
/// no memory row, no chunk, and not even the KB the write would have created.
#[test]
fn a_failed_embed_leaves_no_memory_no_chunk_and_no_kb() {
    let home = tempfile::tempdir().expect("tmp dir");
    let out = engram(home.path())
        .env("ENGRAM_EMBED_FAIL", FAULT)
        .args([
            "remember",
            &multi_chunk_note(),
            "--kb",
            KB,
            "--no-extract-facts",
            "--json",
        ])
        .assert()
        .failure()
        .code(1)
        .get_output()
        .stderr
        .clone();
    let err = parse_error(&out);
    assert_eq!(err["error"]["code"], "transient_error");
    let message = err["error"]["message"].as_str().expect("error message");
    assert!(
        message.contains("ENGRAM_EMBED_FAIL"),
        "the failure must name where it came from: {message}"
    );
    assert!(
        message.contains("re-run"),
        "the failure must name the remedy: {message}"
    );

    let dump = export(home.path());
    assert_eq!(
        dump["data"]["counts"]["memories"], 0,
        "a memory whose chunks never landed must never have been committed"
    );
    assert_eq!(dump["data"]["counts"]["chunks"], 0);

    // The rollback takes the KB registration with it: a failed write must not
    // sprout an empty KB in `kb list`.
    let kbs = run_json(&mut engram(home.path()), &["kb", "list"]);
    assert!(
        kbs["data"]["knowledge_bases"]
            .as_array()
            .expect("kb array")
            .iter()
            .all(|k| k["name"] != KB),
        "a rolled-back write registered its KB anyway: {kbs}"
    );

    let listed = run_json(
        &mut engram(home.path()),
        &["memory", "list", "--diary", "default"],
    );
    assert_eq!(
        listed["data"]["memories"].as_array().map(|a| a.len()),
        Some(0)
    );
}

/// The permanent black hole: with the old code the retry hit the exact-content
/// dedup against the chunkless ghost and reported success while storing
/// nothing, so the content could never be written again.
#[test]
fn a_failed_embed_does_not_poison_the_dedup_check() {
    let home = tempfile::tempdir().expect("tmp dir");
    let content = multi_chunk_note();
    engram(home.path())
        .env("ENGRAM_EMBED_FAIL", FAULT)
        .args([
            "remember",
            &content,
            "--kb",
            KB,
            "--no-extract-facts",
            "--json",
        ])
        .assert()
        .failure();

    // Same content, fault cleared: the retry has to be a real write.
    let mut retry = engram(home.path());
    retry.env_remove("ENGRAM_EMBED_FAIL");
    let v = run_json(
        &mut retry,
        &["remember", &content, "--kb", KB, "--no-extract-facts"],
    );
    assert_eq!(
        v["data"]["stored"], true,
        "the retry was deduped against a memory that was never really stored: {v}"
    );
    assert!(
        v["data"]["deduplicated"].is_null(),
        "nothing was stored the first time, so nothing can be a duplicate: {v}"
    );
    assert!(v["data"]["chunks"].as_u64().expect("chunk count") >= 1);

    let dump = export(home.path());
    assert_eq!(dump["data"]["counts"]["memories"], 1);
    let chunks = dump["data"]["chunks"].as_array().expect("chunk rows");
    assert!(!chunks.is_empty());
    for chunk in chunks {
        assert!(
            !chunk["embedding_b64"].is_null(),
            "a chunk was indexed without a vector: {chunk}"
        );
    }

    let recalled = run_json(
        &mut engram(home.path()),
        &["recall", "Noemi Belgrade summit", "--kb", KB],
    );
    let results = recalled["data"]["results"].as_array().expect("results");
    assert!(
        !results.is_empty(),
        "the retried memory is not retrievable: {recalled}"
    );
}

/// An embed call that never returned was never paid for, so it must not be
/// billed. Runs on the PAID path (a key resolves, no stub) to pin the
/// ordering: the seam fires before `embed_batch`, so nothing is dialled.
#[test]
fn a_failed_embed_records_no_usage_event() {
    let home = tempfile::tempdir().expect("tmp dir");
    let mut cmd = Command::cargo_bin("engram").expect("binary built");
    point_at_home(&mut cmd, home.path());
    cmd.env_remove("ENGRAM_BENCH_FORCE_STUB");
    cmd.env(
        "GEMINI_API_KEY",
        "fake-key-never-used-the-fault-fires-first",
    );
    cmd.env("ENGRAM_EMBED_FAIL", FAULT);
    cmd.args([
        "remember",
        "A paid write that never reached the embedder.",
        "--kb",
        KB,
        "--no-extract-facts",
        "--json",
    ])
    .assert()
    .failure()
    .code(1);

    let usage = run_json(&mut engram(home.path()), &["usage"]);
    assert_eq!(
        usage["data"]["totals"]["events"], 0,
        "an embed that never returned must never be billed: {usage}"
    );

    let dump = export(home.path());
    assert_eq!(dump["data"]["counts"]["memories"], 0);
    assert_eq!(dump["data"]["counts"]["chunks"], 0);
}

/// The reported chunk count now comes from the committed transaction, not from
/// the pre-write plan.
#[test]
fn a_successful_remember_reports_the_chunks_the_transaction_wrote() {
    let home = tempfile::tempdir().expect("tmp dir");
    let v = run_json(
        &mut engram(home.path()),
        &[
            "remember",
            &multi_chunk_note(),
            "--kb",
            KB,
            "--no-extract-facts",
        ],
    );
    let reported = v["data"]["chunks"].as_u64().expect("chunk count");
    assert!(reported >= 1);
    assert_eq!(v["metadata"]["chunks_stored"], reported);

    let dump = export_kb(home.path(), KB);
    assert_eq!(dump["data"]["counts"]["chunks"], reported);
    for chunk in dump["data"]["chunks"].as_array().expect("chunk rows") {
        assert!(
            !chunk["embedding_b64"].is_null(),
            "a chunk was indexed without a vector: {chunk}"
        );
    }

    let shown = run_json(&mut engram(home.path()), &["kb", "show", KB]);
    assert_eq!(shown["data"]["knowledge_base"]["memory_count"], 1);
    assert_eq!(shown["data"]["knowledge_base"]["chunk_count"], reported);
}

/// An accidentally-exported empty variable must not brick every write.
#[test]
fn an_empty_fault_value_is_not_a_fault() {
    for blank in ["", "   "] {
        let home = tempfile::tempdir().expect("tmp dir");
        let mut cmd = engram(home.path());
        cmd.env("ENGRAM_EMBED_FAIL", blank);
        let v = run_json(
            &mut cmd,
            &[
                "remember",
                "A blank fault value means no fault at all.",
                "--kb",
                KB,
                "--no-extract-facts",
            ],
        );
        assert_eq!(v["data"]["stored"], true, "blank {blank:?} blocked a write");
        assert!(v["data"]["chunks"].as_u64().expect("chunk count") >= 1);
    }
}

//! Expiry at recall — a memory whose `valid_until` has passed is withheld
//! from default retrieval, visibly, and `--include-expired` reaches it again.
//!
//! Field-reported failure class: a memory with a natural end date ("event
//! window open until the 11th") stays top-ranked forever because nothing
//! enforces `valid_until`. These tests pin the enforcement AND its
//! loudness: the withholding must be reported under `metadata.expiry`,
//! because a silent thinning of results is the failure mode this store's
//! whole design forbids.
//!
//! Isolated HOME per test; ENGRAM_BENCH_FORCE_STUB so no key is read, no
//! network is touched, and the developer's real store is never opened.

use assert_cmd::Command;
use serde_json::Value;

fn point_at_home(cmd: &mut Command, home: &std::path::Path) {
    cmd.env("HOME", home);
    cmd.env("XDG_CONFIG_HOME", home.join(".config"));
    cmd.env("XDG_DATA_HOME", home.join(".local/share"));
    cmd.env("XDG_CACHE_HOME", home.join(".cache"));
}

fn engram_in(home: &tempfile::TempDir) -> Command {
    let mut cmd = Command::cargo_bin("engram").expect("binary built");
    cmd.env("ENGRAM_BENCH_FORCE_STUB", "1");
    point_at_home(&mut cmd, home.path());
    cmd
}

fn run_json(home: &tempfile::TempDir, args: &[&str]) -> Value {
    let out = engram_in(home)
        .args(args)
        .arg("--json")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    serde_json::from_slice(&out).expect("valid JSON on stdout")
}

fn result_contents(v: &Value) -> Vec<String> {
    v["data"]["results"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|r| r["content"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// The core contract: expired memories leave default recall, visibly, and
/// come back on request.
#[test]
fn expired_memory_is_withheld_by_default_and_reachable_on_request() {
    let home = tempfile::tempdir().unwrap();

    run_json(
        &home,
        &[
            "remember",
            "the quokkavault event window closes soon",
            "--valid-until",
            "2020-01-01",
        ],
    );
    run_json(&home, &["remember", "the quokkavault archive is permanent"]);

    // Default recall: only the unexpired memory, and the exclusion is named.
    let v = run_json(&home, &["recall", "quokkavault"]);
    let contents = result_contents(&v);
    assert!(
        contents.iter().any(|c| c.contains("permanent")),
        "unexpired memory must still be found: {contents:?}"
    );
    assert!(
        !contents.iter().any(|c| c.contains("window closes")),
        "expired memory leaked into default recall: {contents:?}"
    );
    let expiry = &v["data"]["metadata"]["expiry"];
    assert_eq!(
        expiry["expired_memories_excluded"], 1,
        "the withholding must be reported, not silent: {expiry}"
    );
    assert_eq!(expiry["include_expired"], false);

    // Opt back in: the expired memory is reachable again, and the metadata
    // says the gate was lifted rather than pretending nothing was expired.
    let v = run_json(&home, &["recall", "quokkavault", "--include-expired"]);
    let contents = result_contents(&v);
    assert!(
        contents.iter().any(|c| c.contains("window closes")),
        "--include-expired must reach the expired memory: {contents:?}"
    );
    let expiry = &v["data"]["metadata"]["expiry"];
    assert_eq!(expiry["include_expired"], true);
}

/// A future `valid_until` is not expiry: the memory stays in default recall
/// and no expiry note appears (nothing was withheld, nothing to report).
#[test]
fn future_valid_until_is_not_withheld() {
    let home = tempfile::tempdir().unwrap();
    run_json(
        &home,
        &[
            "remember",
            "the quokkavault pass is valid for years",
            "--valid-until",
            "2999-12-31",
        ],
    );
    let v = run_json(&home, &["recall", "quokkavault"]);
    assert!(
        result_contents(&v).iter().any(|c| c.contains("valid for years")),
        "future-dated memory must be found"
    );
    assert!(
        v["data"]["metadata"]["expiry"].is_null(),
        "no expiry note when nothing was withheld: {}",
        v["data"]["metadata"]["expiry"]
    );
}

/// The scope stats must survive the filter: an all-expired scope reads as
/// "the scope held content, all of it expired", never "the scope was empty".
/// Validity and absence are different claims.
#[test]
fn all_expired_scope_is_distinguishable_from_empty_scope() {
    let home = tempfile::tempdir().unwrap();
    run_json(
        &home,
        &[
            "remember",
            "the only quokkavault record, long expired",
            "--valid-until",
            "2020-01-01",
        ],
    );
    let v = run_json(&home, &["recall", "quokkavault"]);
    assert_eq!(result_contents(&v).len(), 0);
    let expiry = &v["data"]["metadata"]["expiry"];
    assert_eq!(expiry["expired_memories_excluded"], 1);
    // The receipt still counts the expired chunk as in-scope.
    let considered = v["data"]["scope"]["chunks_considered"]
        .as_u64()
        .or_else(|| v["data"]["metadata"]["chunks_considered"].as_u64())
        .or_else(|| v["data"]["absence"]["chunks_considered"].as_u64());
    if let Some(considered) = considered {
        assert!(
            considered >= 1,
            "an all-expired scope must not read as empty (considered = {considered})"
        );
    }
}

/// `memory search` shares recall's pipeline and must share the gate.
#[test]
fn memory_search_honors_expiry_too() {
    let home = tempfile::tempdir().unwrap();
    run_json(
        &home,
        &[
            "remember",
            "the quokkavault event window closes soon",
            "--valid-until",
            "2020-01-01",
        ],
    );
    let v = run_json(&home, &["memory", "search", "quokkavault"]);
    assert_eq!(result_contents(&v).len(), 0);
    let v = run_json(
        &home,
        &["memory", "search", "quokkavault", "--include-expired"],
    );
    assert_eq!(result_contents(&v).len(), 1);
}

/// Strictness: a typo'd `--valid-until` is a hard error, never a silent
/// "no expiry".
#[test]
fn malformed_valid_until_is_rejected() {
    let home = tempfile::tempdir().unwrap();
    engram_in(&home)
        .args(["remember", "content", "--valid-until", "next tuesday"])
        .arg("--json")
        .assert()
        .failure();
}

/// A memory that expires before it happened is a contradiction the write
/// path refuses.
#[test]
fn valid_until_before_event_time_is_rejected() {
    let home = tempfile::tempdir().unwrap();
    engram_in(&home)
        .args([
            "remember",
            "content",
            "--event-time",
            "2026-06-01",
            "--valid-until",
            "2026-01-01",
        ])
        .arg("--json")
        .assert()
        .failure();
}

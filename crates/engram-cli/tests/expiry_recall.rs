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
    // A developer's exported ENGRAM_STORE outranks HOME/XDG (cli.rs documents
    // the precedence), so without this line the temp-HOME isolation below is
    // bypassed and mutator tests run against the REAL store.
    cmd.env_remove("ENGRAM_STORE");
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
        result_contents(&v)
            .iter()
            .any(|c| c.contains("valid for years")),
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
    // The absence certificate must say WHY: `all_candidates_expired`, not
    // `no_candidate_matched` — the content exists and --include-expired
    // reaches it. Selling validity withholding as search absence is the H9
    // failure class this feature must not reintroduce.
    assert_eq!(v["data"]["verdict"], "no_match");
    assert_eq!(
        v["data"]["absence"]["reason"], "all_candidates_expired",
        "absence: {}",
        v["data"]["absence"]
    );
    // And the receipt still counts the expired chunk as in-scope: "the scope
    // held content, all of it expired" is a different claim from "empty".
    assert!(
        v["data"]["absence"]["searched_chunks"]
            .as_u64()
            .unwrap_or(0)
            >= 1,
        "an all-expired scope must not read as empty: {}",
        v["data"]["absence"]
    );
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

/// Forgetting the expired source must not REPUBLISH it through any recall
/// mode (re-gate finding 1).
#[test]
fn forgetting_an_expired_memory_does_not_republish_it() {
    let home = tempfile::tempdir().unwrap();
    let v = run_json(
        &home,
        &[
            "remember",
            "the quokkavault window has closed",
            "--valid-until",
            "2020-01-01",
        ],
    );
    let id = v["data"]["id"].as_str().expect("memory id").to_string();
    engram_in(&home)
        .args(["forget", &id, "--confirm"])
        .arg("--json")
        .assert()
        .success();
    // `explore` is a legacy mode name; it must not reach forgotten content.
    let v = run_json(&home, &["recall", "quokkavault", "--mode", "explore"]);
    assert_eq!(
        result_contents(&v).len(),
        0,
        "forgotten expired content leaked back: {:?}",
        result_contents(&v)
    );
}

/// Applying the same `--valid-until` to identical content twice: the second
/// call is a no-op that says so — no second journal event, no false
/// "updated" claim.
#[test]
fn dedup_expiry_is_idempotent() {
    let home = tempfile::tempdir().unwrap();
    run_json(&home, &["remember", "the quokkavault permit"]);
    let v = run_json(
        &home,
        &[
            "remember",
            "the quokkavault permit",
            "--valid-until",
            "2999-06-01",
        ],
    );
    assert_eq!(v["data"]["valid_until_updated"], true);
    let v = run_json(
        &home,
        &[
            "remember",
            "the quokkavault permit",
            "--valid-until",
            "2999-06-01",
        ],
    );
    assert_eq!(
        v["data"]["valid_until_updated"], false,
        "an unchanged expiry must not report a second update"
    );
}

/// Expired high-ranking hits must not starve a pool of one: with three
/// expired matches beside one current one and `--rerank-top-n 1`, the
/// current memory is still found. The decisive lexical-leg pin (arbitrary
/// expired counts vs a pool of one, dense leg out of the picture) lives in
/// the storage unit test `fts_expiry_filter_runs_before_the_limit` — the
/// cutoff runs in FTS SQL before the LIMIT, so it cannot starve at any
/// count.
#[test]
fn expired_hits_cannot_starve_a_small_lexical_pool() {
    let home = tempfile::tempdir().unwrap();
    for i in 1..=3 {
        run_json(
            &home,
            &[
                "remember",
                &format!("starval starval starval expired filler number {i}"),
                "--valid-until",
                "2020-01-01",
            ],
        );
    }
    run_json(&home, &["remember", "starval CURRENT_TARGET row"]);
    let v = run_json(
        &home,
        &["recall", "starval", "--mode", "raw", "--rerank-top-n", "1"],
    );
    let contents = result_contents(&v);
    assert!(
        contents.iter().any(|c| c.contains("CURRENT_TARGET")),
        "expired hits starved the lexical pool: {contents:?}"
    );
}

/// `--include-expired` lifts the validity withholding, and ONLY that: a
/// FORGOTTEN expired memory stays gone — "show me expired" is not "show me
/// retracted" (gate round 3, High).
#[test]
fn include_expired_does_not_expose_forgotten_content() {
    let home = tempfile::tempdir().unwrap();
    let v = run_json(
        &home,
        &[
            "remember",
            "the quokkavault window has closed",
            "--valid-until",
            "2020-01-01",
        ],
    );
    let id = v["data"]["id"].as_str().expect("memory id").to_string();
    engram_in(&home)
        .args(["forget", &id, "--confirm"])
        .arg("--json")
        .assert()
        .success();
    // The forgotten sentence must not come back even when the caller opts
    // into expired content.
    let v = run_json(
        &home,
        &[
            "recall",
            "quokkavault",
            "--mode",
            "explore",
            "--include-expired",
        ],
    );
    assert!(
        !result_contents(&v)
            .iter()
            .any(|c| c.contains("window has closed")),
        "forgotten content exposed via --include-expired: {:?}",
        result_contents(&v)
    );
}

/// A rejected dedup must reject WHOLE: an impossible --valid-until beside a
/// --facts-json batch exits 3 with zero facts persisted (gate round 3,
/// High — validation precedes all persistence).
#[test]
fn rejected_dedup_persists_nothing() {
    let home = tempfile::tempdir().unwrap();
    run_json(
        &home,
        &[
            "remember",
            "the quokkavault permit",
            "--event-time",
            "2025-01-01",
        ],
    );
    engram_in(&home)
        .args([
            "remember",
            "the quokkavault permit",
            "--valid-until",
            "2020-06-01",
            "--facts-json",
            r#"[{"subject":"permit","predicate":"status","object":"active"}]"#,
        ])
        .arg("--json")
        .assert()
        .failure();
    let v = run_json(&home, &["facts", "list"]);
    assert_eq!(
        v["data"]["total"], 0,
        "a rejected dedup persisted facts: {}",
        v["data"]
    );
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

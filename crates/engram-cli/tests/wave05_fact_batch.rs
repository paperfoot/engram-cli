//! Wave 0.5 — one memory's facts are ONE unit of work.
//!
//! `facts backfill` treats a memory as processed once it has ANY fact OR an
//! extraction marker (`PENDING_EXTRACTION_PREDICATE`). Persisting facts one at
//! a time therefore had a state nobody could recover from: fact A committed,
//! fact B failed, no marker was written — and the memory left the backfill
//! queue anyway, on the strength of A. B was lost permanently and silently.
//!
//! The single-fact fixture in `tests/cli.rs` cannot see this: with one fact,
//! "all failed" and "partial" are the same state. Every batch here yields TWO
//! facts and fails on the SECOND.

use assert_cmd::Command;
use serde_json::Value;

/// Point a spawned binary's data/config/cache at `home` so no test can read or
/// write the developer's real memory store.
fn point_at_home(cmd: &mut Command, home: &std::path::Path) {
    // A developer's exported ENGRAM_STORE outranks HOME/XDG (cli.rs documents
    // the precedence), so without this line the temp-HOME isolation below is
    // bypassed and mutator tests run against the REAL store.
    cmd.env_remove("ENGRAM_STORE");
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
    cmd.env_remove("ENGRAM_FACT_LLM_STUB");
    cmd.env_remove("ENGRAM_FACT_WRITE_FAIL");
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

/// Two facts about two different subjects, so no conflict logic is involved —
/// the only thing under test is what survives a mid-batch failure.
const TWO_FACT_SCRIPT: &str = r#"{
  "Lisbon": [
    {"subject":"Ana","predicate":"lives_in","object":"Lisbon","confidence":1.0,"fact_type":"profile_slot"},
    {"subject":"Bruno","predicate":"works_at","object":"Acme","confidence":1.0,"fact_type":"profile_slot"}
  ],
  "*": []
}"#;

const MEMORY: &str = "Ana moved to Lisbon last March and Bruno joined Acme.";

/// `ENGRAM_FACT_WRITE_FAIL` needle: matches fact 2 of 2, never fact 1.
const SECOND_FACT: &str = "Bruno";

const FACTS_JSON: &str = r#"[
  {"subject":"Ana","predicate":"lives_in","object":"Lisbon","confidence":1.0},
  {"subject":"Bruno","predicate":"works_at","object":"Acme","confidence":1.0}
]"#;

fn home() -> tempfile::TempDir {
    tempfile::tempdir().expect("tmp dir")
}

fn fact_count(home: &std::path::Path) -> u64 {
    run_json(
        &mut engram(home),
        &["facts", "list", "--kb", "personal", "--diary", "*"],
    )["data"]["count"]
        .as_u64()
        .expect("a count")
}

fn fact_objects(home: &std::path::Path) -> Vec<String> {
    let v = run_json(
        &mut engram(home),
        &["facts", "list", "--kb", "personal", "--diary", "*"],
    );
    v["data"]["facts"]
        .as_array()
        .expect("facts array")
        .iter()
        .map(|f| f["object"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// THE DEFECT. Fact 1 used to commit and stay committed.
#[test]
fn a_batch_that_fails_on_its_second_fact_stores_neither() {
    let tmp = home();
    let assert = engram(tmp.path())
        .env("ENGRAM_FACT_LLM_STUB", TWO_FACT_SCRIPT)
        .env("ENGRAM_FACT_WRITE_FAIL", SECOND_FACT)
        .args(["remember", MEMORY, "--kb", "personal", "--json"])
        .assert()
        .success();
    let v = parse_stdout(&assert.get_output().stdout);
    // Losing facts must never lose the memory.
    assert_eq!(v["data"]["stored"], true, "{v}");
    assert_eq!(v["data"]["fact_extraction"], "failed_persist", "{v}");
    assert_eq!(v["data"]["facts_added"], 0, "{v}");
    let note = v["data"]["fact_extraction_note"]
        .as_str()
        .expect("a lost batch is never silent");
    assert!(note.contains("fact 2 of 2"), "{note}");
    assert!(note.contains("facts backfill"), "{note}");

    assert_eq!(
        fact_count(tmp.path()),
        0,
        "fact 1 of the batch committed and stayed committed"
    );
}

/// The consequence of the defect: the memory had left the backfill queue
/// forever on the strength of the one fact that landed.
#[test]
fn a_half_written_batch_leaves_the_memory_pending_and_a_clean_retry_recovers_all_of_it() {
    let tmp = home();
    engram(tmp.path())
        .env("ENGRAM_FACT_LLM_STUB", TWO_FACT_SCRIPT)
        .env("ENGRAM_FACT_WRITE_FAIL", SECOND_FACT)
        .args(["remember", MEMORY, "--kb", "personal", "--json"])
        .assert()
        .success();

    let dry = run_json(
        &mut engram(tmp.path()),
        &["facts", "backfill", "--kb", "personal", "--dry-run"],
    );
    assert_eq!(
        dry["data"]["pending"], 1,
        "a memory with one stray fact is invisible to every future backfill"
    );

    let done = run_json(
        engram(tmp.path()).env("ENGRAM_FACT_LLM_STUB", TWO_FACT_SCRIPT),
        &["facts", "backfill", "--kb", "personal"],
    );
    assert_eq!(done["data"]["processed"], 1, "{done}");
    assert_eq!(done["data"]["facts_added"], 2, "{done}");
    let mut objects = fact_objects(tmp.path());
    objects.sort();
    assert_eq!(objects, vec!["Acme".to_string(), "Lisbon".to_string()]);

    // The marker landed inside the committed batch, so a third sweep is a no-op.
    let again = run_json(
        engram(tmp.path()).env("ENGRAM_FACT_LLM_STUB", TWO_FACT_SCRIPT),
        &["facts", "backfill", "--kb", "personal"],
    );
    assert_eq!(again["data"]["processed"], 0, "{again}");
}

/// Guards the marker-inside-the-transaction change: a memory that legitimately
/// yields no facts must still be retired from the queue, or every factless
/// memory is re-sent to the LLM forever.
#[test]
fn a_batch_that_yields_no_facts_is_still_marked_processed() {
    let tmp = home();
    let v = run_json(
        engram(tmp.path()).env("ENGRAM_FACT_LLM_STUB", r#"{"*": []}"#),
        &[
            "remember",
            "A note with nothing extractable.",
            "--kb",
            "personal",
        ],
    );
    assert_eq!(v["data"]["fact_extraction"], "ran_stub", "{v}");
    let dry = run_json(
        &mut engram(tmp.path()),
        &["facts", "backfill", "--kb", "personal", "--dry-run"],
    );
    assert_eq!(dry["data"]["pending"], 0, "{dry}");
}

/// Release-gate finding (Codex): pending used to mean "no fact AND no
/// marker", while a caller-supplied batch commits with `marker: None`
/// precisely because the extractor never saw the memory. One committed caller
/// fact then hid the memory from every future backfill, so the extraction the
/// response promises could never run. Pending is marker-only now: the caller
/// facts commit, the memory STAYS pending, and the next backfill extracts,
/// dedups the re-supplied triples, and retires it with a marker.
#[test]
fn caller_facts_do_not_hide_a_memory_from_extraction() {
    let tmp = home();
    let v = run_json(
        engram(tmp.path()).env("ENGRAM_FACT_LLM_STUB", TWO_FACT_SCRIPT),
        &[
            "remember",
            MEMORY,
            "--kb",
            "personal",
            "--facts-json",
            FACTS_JSON,
        ],
    );
    assert_eq!(v["data"]["fact_extraction"], "skipped_facts_json", "{v}");
    assert_eq!(v["data"]["facts_added"], 2, "{v}");

    // The caller's facts are stored, but no extractor has seen the memory:
    // it must still be on the backfill queue.
    let dry = run_json(
        &mut engram(tmp.path()),
        &["facts", "backfill", "--kb", "personal", "--dry-run"],
    );
    assert_eq!(
        dry["data"]["pending"], 1,
        "a caller-supplied batch must not retire the memory from extraction: {dry}"
    );

    // Backfill extracts (the stub yields the same two triples), dedup absorbs
    // them, the marker retires the memory.
    let run = run_json(
        engram(tmp.path()).env("ENGRAM_FACT_LLM_STUB", TWO_FACT_SCRIPT),
        &["facts", "backfill", "--kb", "personal"],
    );
    assert_eq!(run["data"]["processed"], 1, "{run}");
    assert_eq!(
        fact_count(tmp.path()),
        2,
        "dedup must absorb re-supplied triples"
    );
    let done = run_json(
        &mut engram(tmp.path()),
        &["facts", "backfill", "--kb", "personal", "--dry-run"],
    );
    assert_eq!(done["data"]["pending"], 0, "{done}");
}

/// `facts backfill` re-extracts with an LLM; it never replays triples an agent
/// asserted. A dropped `--facts-json` batch is therefore unrecoverable, and
/// reporting it as success is the lie this closes.
#[test]
fn a_failing_facts_json_batch_exits_non_zero_and_names_the_fact() {
    let tmp = home();
    let assert = engram(tmp.path())
        .env("ENGRAM_FACT_WRITE_FAIL", SECOND_FACT)
        .args([
            "remember",
            MEMORY,
            "--kb",
            "personal",
            "--facts-json",
            FACTS_JSON,
            "--json",
        ])
        .assert()
        .failure()
        .code(1);
    let err = parse_error(&assert.get_output().stderr);
    assert_eq!(err["error"]["code"], "transient_error", "{err}");
    let message = err["error"]["message"].as_str().unwrap_or_default();
    assert!(message.contains("--facts-json"), "{message}");
    assert!(message.contains("fact 2 of 2"), "{message}");
    assert!(message.contains("re-run the identical"), "{message}");

    assert_eq!(
        fact_count(tmp.path()),
        0,
        "nothing from the batch was stored"
    );
}

/// The recovery the error message promises has to exist: re-running the
/// identical command used to be a silent no-op that dropped the triples again.
#[test]
fn the_facts_json_retry_attaches_the_whole_batch_to_the_memory_it_already_stored() {
    let tmp = home();
    engram(tmp.path())
        .env("ENGRAM_FACT_WRITE_FAIL", SECOND_FACT)
        .args([
            "remember",
            MEMORY,
            "--kb",
            "personal",
            "--facts-json",
            FACTS_JSON,
            "--json",
        ])
        .assert()
        .failure();

    let v = run_json(
        &mut engram(tmp.path()),
        &[
            "remember",
            MEMORY,
            "--kb",
            "personal",
            "--facts-json",
            FACTS_JSON,
        ],
    );
    assert_eq!(v["data"]["deduplicated"], true, "{v}");
    assert_eq!(v["data"]["stored"], false, "{v}");
    assert_eq!(v["data"]["facts_added"], 2, "{v}");
    assert_eq!(fact_count(tmp.path()), 2);
}

/// The same rule on the sweep side.
#[test]
fn a_backfill_batch_that_cannot_persist_a_fact_stores_nothing_and_stays_pending() {
    let tmp = home();
    run_json(
        &mut engram(tmp.path()),
        &["remember", MEMORY, "--kb", "personal", "--no-extract-facts"],
    );

    let assert = engram(tmp.path())
        .env("ENGRAM_FACT_LLM_STUB", TWO_FACT_SCRIPT)
        .env("ENGRAM_FACT_WRITE_FAIL", SECOND_FACT)
        .args(["facts", "backfill", "--kb", "personal", "--json"])
        .assert()
        .success();
    let v = parse_stdout(&assert.get_output().stdout);
    assert_eq!(v["data"]["failed"], 1, "{v}");
    assert_eq!(v["data"]["processed"], 0, "{v}");
    assert_eq!(v["data"]["facts_added"], 0, "{v}");
    assert_eq!(v["data"]["remaining_pending"], 1, "{v}");
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).to_string();
    assert!(
        stderr.contains("fact 2 of 2 could not be stored"),
        "the per-memory failure must stay visible: {stderr}"
    );
    assert_eq!(fact_count(tmp.path()), 0);

    let clean = run_json(
        engram(tmp.path()).env("ENGRAM_FACT_LLM_STUB", TWO_FACT_SCRIPT),
        &["facts", "backfill", "--kb", "personal"],
    );
    assert_eq!(clean["data"]["processed"], 1, "{clean}");
    assert_eq!(clean["data"]["facts_added"], 2, "{clean}");
}

/// The injected-failure message used to format the whole `ExtractedFact` — the
/// memory restated — into `fact_extraction_note`, which goes to stdout AND
/// stderr. Only the operator's own needle may be echoed.
#[test]
fn the_injected_failure_never_echoes_the_fact_it_failed_on() {
    const SENTINEL: &str = "zzsecretclinicnamezz";
    let tmp = home();
    let script = format!(
        r#"{{"Lisbon": [
             {{"subject":"Ana","predicate":"lives_in","object":"Lisbon","confidence":1.0}},
             {{"subject":"Bruno","predicate":"works_at","object":"{SENTINEL}","confidence":1.0}}
           ], "*": []}}"#
    );
    let assert = engram(tmp.path())
        .env("ENGRAM_FACT_LLM_STUB", script)
        .env("ENGRAM_FACT_WRITE_FAIL", SECOND_FACT)
        .args(["remember", MEMORY, "--kb", "personal", "--json"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).to_string();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).to_string();
    assert!(
        !stdout.contains(SENTINEL),
        "leaked into the envelope:\n{stdout}"
    );
    assert!(!stderr.contains(SENTINEL), "leaked onto stderr:\n{stderr}");
    // The failure itself must still be reported.
    assert!(stderr.contains("fact 2 of 2"), "{stderr}");
}

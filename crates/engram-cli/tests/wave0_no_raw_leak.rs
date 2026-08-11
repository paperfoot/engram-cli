//! Wave 0.6 — a failed fact extraction must not print the model's answer.
//!
//! The extractor's response restates the memory it was given, so every surface
//! that renders a parse failure is a surface that can copy private content into
//! an agent's transcript. These tests drive the real binary with a scripted
//! offline extractor (`ENGRAM_FACT_LLM_STUB`) whose answer is deliberately
//! malformed AND carries a sentinel, then assert the sentinel reaches neither
//! stdout nor stderr while the diagnostics an operator needs still do.
//!
//! Standalone by design: `tests/cli.rs` is one 5,700-line binary, and each
//! Wave 0 item has to stay independently runnable.

use assert_cmd::Command;
use serde_json::Value;

/// A token that exists nowhere in engram, in this fixture's memory text, or in
/// any other test — so finding it in the output can only mean the model's
/// answer was echoed.
const SENTINEL: &str = "QUOKKA-7F3A-DO-NOT-ECHO";

/// A malformed extraction response hiding the sentinel in BOTH channels a
/// half-fix leaves open: the prose the old `raw:` suffix copied, and the
/// mistyped `confidence` value that serde_json's own Display quotes back
/// (`invalid type: string "…", expected f32`).
///
/// Not valid JSON overall, so `ScriptedChatLlm` returns it verbatim for every
/// call (see `engram_llm::stub::ScriptedChatLlm::parse`).
fn leaky_extraction_script() -> String {
    format!(
        "Sure — here are the facts about {SENTINEL}: \
         [{{\"subject\":\"Ana\",\"predicate\":\"lives_in\",\"object\":\"Lisbon\",\
         \"confidence\":\"{SENTINEL}\"}}]"
    )
}

/// Point a spawned binary's data/config/cache at `home`. The `directories`
/// crate honors XDG_* on Linux and HOME on macOS; a test that forgets one of
/// these reads and WRITES the developer's real memory store.
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

/// A command against `home` that can reach no paid API.
fn engram(home: &tempfile::TempDir) -> Command {
    let mut cmd = Command::cargo_bin("engram").expect("binary built");
    cmd.env("ENGRAM_BENCH_FORCE_STUB", "1");
    point_at_home(&mut cmd, home.path());
    cmd
}

fn parse_stdout(out: &[u8]) -> Value {
    serde_json::from_slice(out).expect("valid JSON on stdout")
}

/// `remember` renders a parse failure into `fact_extraction_note`, which goes
/// to stdout for the agent and stderr for the human. Neither may carry the
/// model's answer — and the failure must stay diagnosable without it.
#[test]
fn a_failed_extraction_reports_the_failure_without_the_model_response() {
    let home = tempfile::tempdir().expect("tmp dir");
    let assert = engram(&home)
        .env("ENGRAM_FACT_LLM_STUB", leaky_extraction_script())
        .args([
            "remember",
            "Ana moved to Lisbon last March.",
            "--kb",
            "personal",
            "--json",
        ])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).to_string();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).to_string();

    assert!(
        !stdout.contains(SENTINEL),
        "the model response leaked into the envelope:\n{stdout}"
    );
    assert!(
        !stderr.contains(SENTINEL),
        "the model response leaked onto stderr:\n{stderr}"
    );

    let v = parse_stdout(&assert.get_output().stdout);
    assert_eq!(
        v["data"]["stored"], true,
        "a fact failure must not lose the memory"
    );
    assert_eq!(v["data"]["fact_extraction"], "failed");
    assert_eq!(v["data"]["facts_added"], 0);
    let note = v["data"]["fact_extraction_note"].as_str().expect("a note");
    for expected in [
        "could not parse fact JSON",
        "line",
        "column",
        "bytes",
        "facts backfill --kb personal",
    ] {
        assert!(note.contains(expected), "the note lost {expected}: {note}");
    }
    assert!(
        stderr.contains("fact extraction failed"),
        "the failure must stay audible: {stderr}"
    );
}

/// Withholding the response must not cost the facts: extraction never
/// succeeded, so no marker was written and the memory is still queued.
#[test]
fn a_withheld_response_leaves_the_memory_recoverable() {
    let home = tempfile::tempdir().expect("tmp dir");
    engram(&home)
        .env("ENGRAM_FACT_LLM_STUB", leaky_extraction_script())
        .args([
            "remember",
            "Ana moved to Lisbon last March.",
            "--kb",
            "personal",
            "--json",
        ])
        .assert()
        .success();

    let out = engram(&home)
        .args([
            "facts",
            "backfill",
            "--kb",
            "personal",
            "--dry-run",
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(
        parse_stdout(&out)["data"]["pending"],
        1,
        "a memory whose extraction failed must stay in the backfill queue"
    );
}

/// `facts backfill` renders the same error on its per-memory progress line —
/// the second surface the raw echo reached, once per memory in a long sweep.
#[test]
fn backfill_progress_reports_a_failed_memory_without_the_model_response() {
    let home = tempfile::tempdir().expect("tmp dir");
    // Seeded with no extractor available, so the memory arrives pending — the
    // pre-activation state a real backfill runs over.
    engram(&home)
        .args([
            "remember",
            "Ana moved to Lisbon last March.",
            "--kb",
            "personal",
            "--json",
        ])
        .assert()
        .success();

    let assert = engram(&home)
        .env("ENGRAM_FACT_LLM_STUB", leaky_extraction_script())
        .args(["facts", "backfill", "--kb", "personal", "--json"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).to_string();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).to_string();
    assert!(
        !stdout.contains(SENTINEL),
        "leaked into the envelope:\n{stdout}"
    );
    assert!(!stderr.contains(SENTINEL), "leaked onto stderr:\n{stderr}");
    assert!(
        stderr.contains("failed (could not parse fact JSON"),
        "the per-memory failure must stay visible: {stderr}"
    );

    let v = parse_stdout(&assert.get_output().stdout);
    assert_eq!(v["data"]["failed"], 1);
    assert_eq!(v["data"]["processed"], 0);
    assert_eq!(v["data"]["remaining_pending"], 1);
}

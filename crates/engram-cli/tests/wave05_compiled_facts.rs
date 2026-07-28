//! Wave 0.5 — a compiled fact goes through the canonical write path.
//!
//! `compile --llm` used to insert its facts directly and blind-supersede every
//! active fact on the same (subject, predicate) with a different object. That
//! retired safety-slot values no path may auto-supersede, destroyed the second
//! value of every multi-valued predicate, and stamped `valid_from` from the
//! compile clock — a fabricated "now" that is the newest date in the store and
//! therefore wins recency supersession against the older true fact.
//!
//! `compile --llm` had no offline seam at all, so none of it was ever
//! exercised. `ENGRAM_COMPILER_LLM_STUB` is that seam; these tests drive the
//! whole compiler LLM path against an isolated HOME with no provider key
//! reachable.

use assert_cmd::Command;
use serde_json::Value;

/// Point a spawned binary's data/config/cache at `home` so no test can read or
/// write the developer's real memory store, and strip every provider key the
/// developer's own shell might export.
fn point_at_home(cmd: &mut Command, home: &std::path::Path) {
    cmd.env_remove("GEMINI_API_KEY");
    cmd.env_remove("COHERE_API_KEY");
    cmd.env_remove("OPENROUTER_API_KEY");
    cmd.env_remove("ENGRAM_COMPILER_LLM_STUB");
    cmd.env_remove("ENGRAM_FACT_LLM_STUB");
    cmd.env("HOME", home);
    cmd.env("XDG_CONFIG_HOME", home.join(".config"));
    cmd.env("XDG_DATA_HOME", home.join(".local/share"));
    cmd.env("XDG_CACHE_HOME", home.join(".cache"));
}

fn engram_in(home: &std::path::Path) -> Command {
    let mut cmd = Command::cargo_bin("engram").expect("binary built");
    cmd.env("ENGRAM_BENCH_FORCE_STUB", "1");
    point_at_home(&mut cmd, home);
    cmd
}

fn isolated_home() -> tempfile::TempDir {
    tempfile::tempdir().expect("tmp dir")
}

fn cmd_in(tmp: &tempfile::TempDir) -> Command {
    engram_in(tmp.path())
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
    serde_json::from_slice(&out).expect("valid JSON on stdout")
}

const KB: &str = "corpus";

/// One scripted compiler-LLM rule: a substring of the prompt (which carries the
/// chunk) and the `LlmEvidence` object it should answer with. Note the shape is
/// the evidence OBJECT, not the bare fact array `ENGRAM_FACT_LLM_STUB` uses.
fn facts_rule(key: &str, subject: &str, predicate: &str, object: &str) -> String {
    format!(
        r#""{key}": {{"facts":[{{"subject":"{subject}","predicate":"{predicate}","object":"{object}","confidence":0.9}}]}}"#
    )
}

/// Wrap rules into a script whose default answer yields nothing, so a chunk no
/// rule names contributes no facts.
fn compiler_stub(rules: &[String]) -> String {
    format!(r#"{{{}, "*": {{"facts":[]}}}}"#, rules.join(", "))
}

fn compile_llm(tmp: &tempfile::TempDir, script: &str) -> Value {
    let mut cmd = cmd_in(tmp);
    cmd.env("ENGRAM_COMPILER_LLM_STUB", script);
    run_json(&mut cmd, &["compile", "--kb", KB, "--all", "--llm"])
}

/// Store a memory without letting the extractor near it: these tests are about
/// what the COMPILER writes.
fn seed(tmp: &tempfile::TempDir, content: &str, event_time: Option<&str>) {
    let mut args = vec!["remember", content, "--kb", KB, "--no-extract-facts"];
    if let Some(t) = event_time {
        args.push("--event-time");
        args.push(t);
    }
    run_json(&mut cmd_in(tmp), &args);
}

/// Store a memory plus a caller-supplied fact — the "already in the store"
/// value a compiled fact must not silently retire.
fn seed_with_fact(tmp: &tempfile::TempDir, content: &str, facts_json: &str) {
    run_json(
        &mut cmd_in(tmp),
        &[
            "remember",
            content,
            "--kb",
            KB,
            "--no-extract-facts",
            "--facts-json",
            facts_json,
        ],
    );
}

fn facts_show(tmp: &tempfile::TempDir, subject: &str) -> Value {
    run_json(
        &mut cmd_in(tmp),
        &["facts", "show", subject, "--kb", KB, "--diary", "*"],
    )
}

fn open_conflicts(tmp: &tempfile::TempDir) -> Value {
    run_json(
        &mut cmd_in(tmp),
        &["facts", "conflicts", "--open", "--kb", KB],
    )
}

/// The defect itself: a compiled fact used to be stamped `Utc::now()` twice.
#[test]
fn a_compiled_fact_is_dated_by_its_memory_not_by_the_compile_clock() {
    let tmp = isolated_home();
    seed(
        &tmp,
        "Ana lived in London in a flat near the river.",
        Some("2015-06-01"),
    );
    let script = compiler_stub(&[facts_rule("London", "Ana", "lives_in", "London")]);
    let compiled = compile_llm(&tmp, &script);
    assert_eq!(compiled["data"]["compile"]["llm_facts"], 1, "{compiled}");

    let listed = run_json(
        &mut cmd_in(&tmp),
        &[
            "facts",
            "list",
            "--subject",
            "Ana",
            "--kb",
            KB,
            "--diary",
            "*",
        ],
    );
    assert_eq!(listed["data"]["count"], 1, "{listed}");
    let fact = &listed["data"]["facts"][0];
    assert_eq!(fact["source_kind"], "compile_llm", "{fact}");
    let valid_from = fact["valid_from"].as_str().expect("a dated fact");
    assert!(
        valid_from.starts_with("2015-06-01"),
        "a compiled fact is dated by its memory, got {valid_from}"
    );
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    assert!(
        !valid_from.starts_with(&today),
        "a fabricated `now` is the newest date in the store and wins recency \
         supersession against the older true fact"
    );
}

/// No path may auto-supersede an allergy, a medication, a diagnosis or a blood
/// type. The compiler used to do exactly that, tagging it `exact_match`.
#[test]
fn a_compiled_fact_never_retires_a_safety_slot() {
    let tmp = isolated_home();
    seed_with_fact(
        &tmp,
        "Clinic intake notes for Ana were filed this morning.",
        r#"[{"subject":"Ana","predicate":"allergic_to","object":"penicillin","confidence":1.0}]"#,
    );
    seed(
        &tmp,
        "Ana is allergic to amoxicillin according to the clinic chart.",
        None,
    );
    let script = compiler_stub(&[facts_rule(
        "amoxicillin",
        "Ana",
        "allergic_to",
        "amoxicillin",
    )]);
    let compiled = compile_llm(&tmp, &script);
    assert_eq!(compiled["data"]["compile"]["llm_facts"], 1, "{compiled}");
    assert_eq!(
        compiled["data"]["compile"]["llm_facts_queued"], 1,
        "a contradiction the compiler refuses to settle must be reported, not silent"
    );

    let shown = facts_show(&tmp, "Ana");
    assert_eq!(shown["data"]["active_count"], 2, "{shown}");
    assert_eq!(
        shown["data"]["historic_count"], 0,
        "penicillin was retired by a compile — this is a medical incident, not a stale profile field"
    );

    let conflicts = open_conflicts(&tmp);
    assert_eq!(conflicts["data"]["total_open"], 1, "{conflicts}");
    assert_eq!(
        conflicts["data"]["open_conflicts"][0]["triage"]["rule"], "safety_slot",
        "{conflicts}"
    );
}

/// `Ana knows Bob` is not a correction of `Ana knows Alice`.
#[test]
fn a_compiled_fact_adds_a_second_value_instead_of_destroying_the_first() {
    let tmp = isolated_home();
    seed_with_fact(
        &tmp,
        "Ana and Alice have known each other since university.",
        r#"[{"subject":"Ana","predicate":"knows","object":"Alice","confidence":1.0}]"#,
    );
    seed(
        &tmp,
        "Ana knows Bob from the fund and they worked together for years.",
        None,
    );
    let script = compiler_stub(&[facts_rule("Bob from the fund", "Ana", "knows", "Bob")]);
    compile_llm(&tmp, &script);

    let shown = facts_show(&tmp, "Ana");
    assert_eq!(shown["data"]["active_count"], 2, "{shown}");
    assert_eq!(shown["data"]["historic_count"], 0, "{shown}");
    let conflicts = open_conflicts(&tmp);
    assert_eq!(
        conflicts["data"]["total_open"], 0,
        "a multi-valued addition is neither a supersession nor a question"
    );
}

/// Chunks are compiled in `ORDER BY position, id` — a UUID tiebreak for
/// one-chunk memories. Which of two papers owns a slot must not depend on it.
#[test]
fn two_sources_the_compiler_cannot_order_are_queued_not_decided_by_chunk_order() {
    let tmp = isolated_home();
    // BOTH event times are pinned: relying on wall-clock `created_at` would make
    // the whole-second gap between two spawned processes a coin toss against
    // the classifier's one-second recency margin.
    seed(
        &tmp,
        "Ana lives in Lisbon and walks to the office every morning.",
        Some("2024-01-01"),
    );
    seed(
        &tmp,
        "Ana lives in Porto and takes the train down the coast.",
        Some("2024-01-01"),
    );
    let script = compiler_stub(&[
        facts_rule("Lisbon", "Ana", "lives_in", "Lisbon"),
        facts_rule("Porto", "Ana", "lives_in", "Porto"),
    ]);
    compile_llm(&tmp, &script);

    let shown = facts_show(&tmp, "Ana");
    assert_eq!(shown["data"]["active_count"], 2, "{shown}");
    assert_eq!(shown["data"]["historic_count"], 0, "{shown}");
    let conflicts = open_conflicts(&tmp);
    assert_eq!(conflicts["data"]["total_open"], 1, "{conflicts}");
    assert_eq!(
        conflicts["data"]["open_conflicts"][0]["triage"]["rule"], "comparable_values",
        "{conflicts}"
    );
}

/// The compiler still resolves what the sources' own dates order — and the
/// audit trail says the COMPILER did it, not "the user told us so".
#[test]
fn an_ordered_slot_still_resolves_and_the_audit_trail_names_the_compiler() {
    let tmp = isolated_home();
    seed(
        &tmp,
        "Ana lived in London in a flat near the river.",
        Some("2015-06-01"),
    );
    seed(
        &tmp,
        "Ana lives in Lisbon and walks to the office every morning.",
        Some("2024-02-01"),
    );
    let script = compiler_stub(&[
        facts_rule("London", "Ana", "lives_in", "London"),
        facts_rule("Lisbon", "Ana", "lives_in", "Lisbon"),
    ]);
    compile_llm(&tmp, &script);

    let shown = facts_show(&tmp, "Ana");
    assert_eq!(shown["data"]["active_count"], 1, "{shown}");
    assert_eq!(shown["data"]["active"][0]["object"], "Lisbon", "{shown}");
    assert_eq!(shown["data"]["historic_count"], 1, "{shown}");
    assert_eq!(shown["data"]["historic"][0]["object"], "London", "{shown}");
    let resolution = shown["data"]["historic"][0]["resolution"]
        .as_str()
        .expect("a supersession records who decided");
    // Chunk order is a UUID tiebreak, so either direction is legitimate.
    assert!(
        resolution.starts_with("compile_"),
        "a compile supersession must be distinguishable from a write-time one, got {resolution}"
    );
    assert_eq!(open_conflicts(&tmp)["data"]["total_open"], 0);
}

/// A recompile of unchanged text must converge, not grow.
#[test]
fn recompiling_the_same_corpus_writes_no_new_facts_and_queues_nothing_new() {
    let tmp = isolated_home();
    seed_with_fact(
        &tmp,
        "Clinic intake notes for Ana were filed this morning.",
        r#"[{"subject":"Ana","predicate":"allergic_to","object":"penicillin","confidence":1.0}]"#,
    );
    seed(
        &tmp,
        "Ana is allergic to amoxicillin according to the clinic chart.",
        None,
    );
    let script = compiler_stub(&[facts_rule(
        "amoxicillin",
        "Ana",
        "allergic_to",
        "amoxicillin",
    )]);
    compile_llm(&tmp, &script);
    let active_after_first = facts_show(&tmp, "Ana")["data"]["active_count"].clone();
    let open_after_first = open_conflicts(&tmp)["data"]["total_open"].clone();

    let second = compile_llm(&tmp, &script);
    assert_eq!(second["data"]["compile"]["llm_facts"], 0, "{second}");
    assert_eq!(second["data"]["compile"]["llm_facts_queued"], 0, "{second}");
    assert_eq!(
        facts_show(&tmp, "Ana")["data"]["active_count"],
        active_after_first
    );
    assert_eq!(open_conflicts(&tmp)["data"]["total_open"], open_after_first);
}

/// The offline seam must not weaken the refusal a real user relies on.
#[test]
fn compile_llm_without_a_key_or_a_script_is_still_a_config_error() {
    let tmp = isolated_home();
    seed(&tmp, "Ana lived in London in a flat near the river.", None);
    let out = cmd_in(&tmp)
        .args(["compile", "--kb", KB, "--all", "--llm", "--json"])
        .assert()
        .failure()
        .code(2)
        .get_output()
        .stderr
        .clone();
    let line = String::from_utf8_lossy(&out)
        .lines()
        .rev()
        .find(|l| l.trim_start().starts_with('{'))
        .expect("JSON error on stderr")
        .to_string();
    let err: Value = serde_json::from_str(&line).expect("parseable error envelope");
    assert_eq!(err["error"]["code"], "config_error", "{err}");
    assert!(
        err["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("OPENROUTER_API_KEY"),
        "{err}"
    );
}

//! Wave 0.2 — an extracted date the store cannot read must never become today.
//!
//! A fabricated "now" is not a missing date: it is the NEWEST date in the
//! store, so it wins recency supersession against the older true fact and
//! retires real history over a string the parser could not read. These tests
//! spawn the built binary against an isolated HOME, exactly as `tests/cli.rs`
//! does, so the developer's real store is never touched.

use assert_cmd::Command;
use serde_json::Value;

/// A binary pinned to `home`, with no API key reachable:
/// ENGRAM_BENCH_FORCE_STUB routes embedding to the offline stub and makes the
/// fact layer report "no LLM" rather than calling OpenRouter.
fn engram_in(home: &std::path::Path) -> Command {
    let mut cmd = Command::cargo_bin("engram").expect("binary built");
    cmd.env("ENGRAM_BENCH_FORCE_STUB", "1");
    cmd.env("HOME", home);
    cmd.env("XDG_CONFIG_HOME", home.join(".config"));
    cmd.env("XDG_DATA_HOME", home.join(".local/share"));
    cmd.env("XDG_CACHE_HOME", home.join(".cache"));
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

/// The one date-shaped string the parser is guaranteed to refuse
/// (`engram_core::temporal` accepts RFC3339 and bare YYYY-MM-DD only).
const UNREADABLE: &str = "last March";

/// Storage keeps full timestamps; this test suite only ever cares which DAY a
/// fact was dated, so compare the `YYYY-MM-DD` prefix.
fn on_day(value: &Value, day: &str) -> bool {
    value.as_str().map(|s| s.starts_with(day)).unwrap_or(false)
}

fn facts_json(object: &str, valid_from: &str) -> String {
    format!(
        r#"[{{"subject":"Elias","predicate":"lives_in","object":"{object}","confidence":1.0,"valid_from":"{valid_from}"}}]"#
    )
}

/// The defect itself: an unreadable date is inherited from the memory, and the
/// caller is told so instead of the store silently dating the fact today.
#[test]
fn an_unreadable_date_is_taken_from_the_memory_not_the_clock() {
    let tmp = isolated_home();
    let v = run_json(
        &mut cmd_in(&tmp),
        &[
            "remember",
            "Elias lived in London back in 2015.",
            "--kb",
            "personal",
            "--event-time",
            "2015-06-01",
            "--facts-json",
            &facts_json("London", UNREADABLE),
        ],
    );
    let warnings = v["data"]["date_parse_warnings"]
        .as_array()
        .expect("date_parse_warnings array");
    assert_eq!(warnings.len(), 1, "{v}");
    let w = &warnings[0];
    assert_eq!(w["field"], "valid_from");
    assert_eq!(w["unreadable_value"], UNREADABLE);
    assert_eq!(w["fallback"], "memory_event_time");
    assert!(
        !w["hint"].as_str().unwrap_or("").is_empty(),
        "a warning without a remedy is noise: {w}"
    );
    assert_eq!(v["metadata"]["date_parse_warnings"], 1);

    let shown = run_json(
        &mut cmd_in(&tmp),
        &["facts", "show", "Elias", "--kb", "personal", "--diary", "*"],
    );
    assert!(
        on_day(&shown["data"]["active"][0]["valid_from"], "2015-06-01"),
        "the fact was dated from the clock, not from its memory: {shown}"
    );
}

/// The consequence the fix exists for: a fabricated "now" made a 2015 memory
/// beat the current value on recency. It must lose instead.
#[test]
fn a_fabricated_date_no_longer_beats_the_true_current_value() {
    let tmp = isolated_home();
    run_json(
        &mut cmd_in(&tmp),
        &[
            "remember",
            "Elias moved to Lisbon in 2024.",
            "--kb",
            "personal",
            "--facts-json",
            &facts_json("Lisbon", "2024-02-01"),
        ],
    );
    let v = run_json(
        &mut cmd_in(&tmp),
        &[
            "remember",
            "Elias lived in London back in 2015.",
            "--kb",
            "personal",
            "--event-time",
            "2015-06-01",
            "--facts-json",
            &facts_json("London", UNREADABLE),
        ],
    );
    assert_eq!(
        v["data"]["adjudication_needed"].as_array().map(|a| a.len()),
        Some(0),
        "an ordered slot needs no human: {v}"
    );
    let conflicts = v["data"]["conflicts"].as_array().expect("conflicts array");
    assert_eq!(conflicts.len(), 1, "{v}");
    assert_eq!(
        conflicts[0]["resolution"], "recency_stale",
        "the older memory's fact was allowed to retire the current value: {v}"
    );
    assert_eq!(conflicts[0]["old_object"], "London");
    assert_eq!(conflicts[0]["new_object"], "Lisbon");

    let shown = run_json(
        &mut cmd_in(&tmp),
        &["facts", "show", "Elias", "--kb", "personal", "--diary", "*"],
    );
    assert_eq!(shown["data"]["active_count"], 1, "{shown}");
    assert_eq!(shown["data"]["active"][0]["object"], "Lisbon");
    assert_eq!(shown["data"]["historic"][0]["object"], "London");
}

/// With no `--event-time` the anchor is when the store recorded the memory —
/// still the memory's own date, never a second clock read at fact-write time.
#[test]
fn without_an_event_time_the_fallback_is_the_memory_record_not_a_second_clock_read() {
    let tmp = isolated_home();
    let v = run_json(
        &mut cmd_in(&tmp),
        &[
            "remember",
            "Elias lived in London at some point.",
            "--kb",
            "personal",
            "--facts-json",
            &facts_json("London", UNREADABLE),
        ],
    );
    let w = &v["data"]["date_parse_warnings"][0];
    assert_eq!(w["fallback"], "memory_created_at", "{v}");
    let used_day = w["used_instead"]
        .as_str()
        .expect("used_instead")
        .get(..10)
        .expect("YYYY-MM-DD prefix")
        .to_string();

    let shown = run_json(
        &mut cmd_in(&tmp),
        &["facts", "show", "Elias", "--kb", "personal", "--diary", "*"],
    );
    assert!(
        on_day(&shown["data"]["active"][0]["valid_from"], &used_day),
        "the envelope reported a date the store does not hold: {shown}"
    );
}

/// The guard against over-firing: a date the parser CAN read is stored
/// verbatim, outranks the memory's event time, and reports nothing.
#[test]
fn a_readable_date_is_stored_verbatim_and_reports_nothing() {
    let tmp = isolated_home();
    let v = run_json(
        &mut cmd_in(&tmp),
        &[
            "remember",
            "Elias moved to Porto.",
            "--kb",
            "personal",
            "--event-time",
            "2015-06-01",
            "--facts-json",
            &facts_json("Porto", "2026-03-01"),
        ],
    );
    assert_eq!(
        v["data"]["date_parse_warnings"].as_array().map(|a| a.len()),
        Some(0),
        "{v}"
    );
    assert_eq!(v["metadata"]["date_parse_warnings"], 0);

    let shown = run_json(
        &mut cmd_in(&tmp),
        &["facts", "show", "Elias", "--kb", "personal", "--diary", "*"],
    );
    assert!(on_day(
        &shown["data"]["active"][0]["valid_from"],
        "2026-03-01"
    ));
}

/// `born_on 1990-04-12` was true in 1990 whatever the memory's date is — that
/// precedence survives, and the unreadable string is still reported.
#[test]
fn a_date_valued_object_still_outranks_an_unreadable_valid_from() {
    let tmp = isolated_home();
    let v = run_json(
        &mut cmd_in(&tmp),
        &[
            "remember",
            "Ana was born in the spring of 1990.",
            "--kb",
            "personal",
            "--event-time",
            "2020-01-01",
            "--facts-json",
            r#"[{"subject":"Ana","predicate":"born_on","object":"1990-04-12","confidence":1.0,"valid_from":"last March"}]"#,
        ],
    );
    let w = &v["data"]["date_parse_warnings"][0];
    assert_eq!(w["fallback"], "date_valued_object", "{v}");
    assert_eq!(w["unreadable_value"], UNREADABLE);

    let shown = run_json(
        &mut cmd_in(&tmp),
        &["facts", "show", "Ana", "--kb", "personal", "--diary", "*"],
    );
    assert!(
        on_day(&shown["data"]["active"][0]["valid_from"], "1990-04-12"),
        "{shown}"
    );
}

/// A backfilled memory is usually OLD. Its facts must inherit its own date,
/// not the date of the sweep that finally read it — and `facts backfill`
/// reports the same warning `remember` does.
#[test]
fn the_extractor_path_reports_the_same_warning_as_facts_json() {
    const LISBON_UNREADABLE: &str = r#"{
      "Lisbon": [{"subject":"Ana","predicate":"lives_in","object":"Lisbon","confidence":1.0,"valid_from":"last March"}],
      "*": []
    }"#;

    let tmp = isolated_home();
    // No LLM reachable, so the memory is stored and left pending.
    run_json(
        &mut cmd_in(&tmp),
        &[
            "remember",
            "Ana moved to Lisbon last March.",
            "--kb",
            "personal",
            "--event-time",
            "2015-06-01",
        ],
    );

    let mut backfill = cmd_in(&tmp);
    backfill.env("ENGRAM_FACT_LLM_STUB", LISBON_UNREADABLE);
    let v = run_json(&mut backfill, &["facts", "backfill", "--kb", "personal"]);
    assert_eq!(
        v["data"]["date_parse_warnings"].as_array().map(|a| a.len()),
        Some(1),
        "{v}"
    );
    assert_eq!(v["metadata"]["date_parse_warnings"], 1);

    let facts = run_json(
        &mut cmd_in(&tmp),
        &["facts", "list", "--subject", "Ana", "--kb", "personal"],
    );
    assert!(
        on_day(&facts["data"]["facts"][0]["valid_from"], "2015-06-01"),
        "a backfilled fact was dated by the sweep, not by its memory: {facts}"
    );
}

/// The envelope contract: `metadata` carries the count, `data` the array —
/// the same split `conflicts`/`conflicts_detected` already uses.
#[test]
fn metadata_count_matches_the_reported_array() {
    let tmp = isolated_home();
    let v = run_json(
        &mut cmd_in(&tmp),
        &[
            "remember",
            "Elias lived in London back in 2015.",
            "--kb",
            "personal",
            "--event-time",
            "2015-06-01",
            "--facts-json",
            &facts_json("London", UNREADABLE),
        ],
    );
    let len = v["data"]["date_parse_warnings"]
        .as_array()
        .expect("array")
        .len();
    assert_eq!(v["metadata"]["date_parse_warnings"], len);
}

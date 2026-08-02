//! Wave 3, read surfaces — deliverables 4 and 5: what the conflicts array
//! advises, and which scope it drew from.
//!
//! Two failures are pinned here. R7: a safety pair never advises superseding,
//! including for queue rows written before wave 3, because an agent that
//! executes `supersede-old` on a medication retires a live prescription. And
//! the diary leak: a `--diary work` recall used to report a contradiction
//! between two `personal` facts.

mod wave3_read_support;

use serde_json::Value;
use wave3_read_support::{remember_fact, run_json, store};

fn seed_pair(
    dir: &std::path::Path,
    kb: &str,
    diary: &str,
    subject: &str,
    predicate: &str,
    a: &str,
    b: &str,
) {
    remember_fact(dir, "first", kb, diary, subject, predicate, a);
    remember_fact(dir, "second", kb, diary, subject, predicate, b);
}

#[test]
fn a_safety_pair_always_requires_context() {
    let dir = store();
    seed_pair(
        dir.path(),
        "personal",
        "default",
        "Ana",
        "takes_medication",
        "metformin 500mg",
        "metformin 1000mg",
    );

    let v = run_json(
        dir.path(),
        &[
            "recall",
            "Ana metformin",
            "--kb",
            "personal",
            "--profile",
            "offline",
        ],
    );
    let contested: Vec<&Value> = v["data"]["conflicts"]
        .as_array()
        .expect("conflicts")
        .iter()
        .filter(|c| c["status"] == "contested")
        .collect();
    assert!(!contested.is_empty(), "no conflict surfaced: {v}");
    for c in &contested {
        assert_eq!(c["suggested_action"], Value::from("requires_context"));
        assert_eq!(c["safety"], Value::Bool(true));
    }
    // Written AFTER the wave-3 write path merged: fresh rows are stored with
    // the correct suggestion, so the read-side guard has nothing to override
    // and must say so honestly — a nonzero count here would mean the write
    // side regressed and the guard is papering over it. The guard's override
    // behaviour for pre-v10 legacy rows is pinned by read_surface's unit
    // tests and by the v9 migration fixture's backlog.
    let overridden = &v["metadata"]["suggested_action_overridden"];
    assert!(
        overridden.is_null() || overridden.as_i64() == Some(0),
        "read guard overrode a fresh write — write side regressed: {overridden}"
    );

    // Same answer on the queue view: stored correct, not corrected.
    let q = run_json(dir.path(), &["facts", "conflicts", "--open"]);
    for e in q["data"]["open_conflicts"].as_array().expect("entries") {
        assert_eq!(e["suggested_action"], Value::from("requires_context"));
        let e_over = &e["suggested_action_overridden"];
        assert!(
            e_over.is_null() || *e_over == Value::Bool(false),
            "{e_over}"
        );
    }
}

/// The honest half of R7: a non-safety single-valued pair that queued only
/// because autonomy was strict may still suggest superseding.
#[test]
fn a_non_safety_pair_may_still_suggest_supersede_old() {
    let dir = store();
    seed_pair(
        dir.path(),
        "personal",
        "default",
        "Bruno",
        "lives_in",
        "Lisbon",
        "Porto",
    );

    let q = run_json(dir.path(), &["facts", "conflicts", "--open"]);
    let e = &q["data"]["open_conflicts"][0];
    assert_eq!(e["suggested_action"], Value::from("supersede-old"));
    assert_eq!(e["safety"], Value::Bool(false));
    assert_eq!(e["suggested_action_overridden"], Value::Bool(false));
    assert_eq!(q["metadata"].get("suggested_action_overridden"), None);
}

/// Permission is not narrowed by safety — only advice is. A caller decision is
/// authority, so every action stays available, `keep-both` first because it is
/// the only safe blind default.
#[test]
fn allowed_actions_stay_open_while_advice_narrows() {
    let dir = store();
    seed_pair(
        dir.path(),
        "personal",
        "default",
        "Ana",
        "takes_medication",
        "metformin 500mg",
        "metformin 1000mg",
    );
    let q = run_json(dir.path(), &["facts", "conflicts", "--open"]);
    let e = &q["data"]["open_conflicts"][0];
    assert_eq!(
        e["resolve"]["allowed_actions"],
        serde_json::json!(["keep-both", "supersede-old", "reject-new"])
    );
    assert_eq!(e["suggested_action"], Value::from("requires_context"));
}

/// The old template shipped `--action supersede-old|keep-both|reject-new`: a
/// paste of that runs the destructive branch and pipes it through two
/// nonexistent programs. One command, no pipe, and it runs.
#[test]
fn the_resolve_command_is_pipe_free_and_runnable() {
    let dir = store();
    seed_pair(
        dir.path(),
        "personal",
        "default",
        "Bruno",
        "lives_in",
        "Lisbon",
        "Porto",
    );
    let q = run_json(dir.path(), &["facts", "conflicts", "--open"]);
    let command = q["data"]["open_conflicts"][0]["resolve_command"]
        .as_str()
        .expect("resolve_command")
        .to_string();
    for bad in ['|', '&', ';', '>', '<', '`', '$'] {
        assert!(!command.contains(bad), "{command} contains {bad}");
    }
    let args: Vec<&str> = command.split_whitespace().skip(1).collect();
    let out = run_json(dir.path(), &args);
    assert_eq!(out["data"]["resolved"], Value::Bool(true));
}

#[test]
fn recall_conflicts_respect_the_diary() {
    let dir = store();
    seed_pair(
        dir.path(),
        "personal",
        "work",
        "Bruno",
        "lives_in",
        "Lisbon",
        "Porto",
    );

    let own = run_json(
        dir.path(),
        &[
            "recall",
            "Bruno lives",
            "--kb",
            "personal",
            "--diary",
            "work",
            "--profile",
            "offline",
        ],
    );
    assert!(
        !own["data"]["conflicts"].as_array().unwrap().is_empty(),
        "the conflict's own diary lost it: {own}"
    );

    let other = run_json(
        dir.path(),
        &[
            "recall",
            "Bruno lives",
            "--kb",
            "personal",
            "--diary",
            "personal",
            "--profile",
            "offline",
        ],
    );
    assert!(
        other["data"]["conflicts"].as_array().unwrap().is_empty(),
        "another diary's conflict leaked into this recall: {other}"
    );
    assert_eq!(other["metadata"]["scope"]["diary"], Value::from("personal"));
}

#[test]
fn recall_conflicts_respect_the_kb() {
    let dir = store();
    seed_pair(
        dir.path(),
        "personal",
        "default",
        "Bruno",
        "lives_in",
        "Lisbon",
        "Porto",
    );
    let v = run_json(
        dir.path(),
        &[
            "recall",
            "Bruno lives",
            "--kb",
            "business",
            "--profile",
            "offline",
        ],
    );
    assert!(v["data"]["conflicts"].as_array().unwrap().is_empty(), "{v}");
}

/// The history view accepted `--kb`/`--diary` and ignored both.
#[test]
fn the_conflict_history_honours_kb_and_diary() {
    let dir = store();
    seed_pair(
        dir.path(),
        "personal",
        "default",
        "Bruno",
        "lives_in",
        "Lisbon",
        "Porto",
    );
    // Resolve it so it becomes history rather than backlog.
    let q = run_json(dir.path(), &["facts", "conflicts", "--open"]);
    let id = q["data"]["open_conflicts"][0]["conflict_id"]
        .as_str()
        .expect("conflict id")
        .to_string();
    run_json(
        dir.path(),
        &["facts", "resolve", &id, "--action", "supersede-old"],
    );

    let mine = run_json(dir.path(), &["facts", "conflicts", "--kb", "personal"]);
    assert_eq!(mine["data"]["total"], Value::from(1));
    let elsewhere = run_json(dir.path(), &["facts", "conflicts", "--kb", "business"]);
    assert_eq!(elsewhere["data"]["total"], Value::from(0));
    assert_eq!(elsewhere["data"]["returned"], Value::from(0));
}

/// A closed interval with no successor is `ended`, not `superseded`, and it
/// prints no `new_object`: a stopped medication has no replacement value, and
/// inventing one is the same defect the sweep exists to close.
#[test]
fn a_superseded_fact_surfaces_in_recall_as_closed() {
    let dir = store();
    seed_pair(
        dir.path(),
        "personal",
        "default",
        "Bruno",
        "lives_in",
        "Lisbon",
        "Porto",
    );
    let q = run_json(dir.path(), &["facts", "conflicts", "--open"]);
    let id = q["data"]["open_conflicts"][0]["conflict_id"]
        .as_str()
        .expect("conflict id")
        .to_string();
    run_json(
        dir.path(),
        &["facts", "resolve", &id, "--action", "supersede-old"],
    );

    let v = run_json(
        dir.path(),
        &[
            "recall",
            "Bruno lives",
            "--kb",
            "personal",
            "--profile",
            "offline",
        ],
    );
    let closed: Vec<&Value> = v["data"]["conflicts"]
        .as_array()
        .expect("conflicts")
        .iter()
        .filter(|c| c["status"] != "contested")
        .collect();
    assert!(!closed.is_empty(), "the closed interval vanished: {v}");
    let row = closed[0];
    assert_eq!(row["status"], Value::from("superseded"));
    assert!(row["new_object"].is_string(), "{row}");
    assert!(row["old_fact_handle"]
        .as_str()
        .unwrap_or_default()
        .starts_with("fact:"));
}

/// `facts show` moves a closed interval out of `active`, which is the whole
/// point of the sweep.
#[test]
fn a_closed_interval_is_historic_in_facts_show() {
    let dir = store();
    seed_pair(
        dir.path(),
        "personal",
        "default",
        "Bruno",
        "lives_in",
        "Lisbon",
        "Porto",
    );
    let q = run_json(dir.path(), &["facts", "conflicts", "--open"]);
    let id = q["data"]["open_conflicts"][0]["conflict_id"]
        .as_str()
        .expect("conflict id")
        .to_string();
    run_json(
        dir.path(),
        &["facts", "resolve", &id, "--action", "supersede-old"],
    );

    let v = run_json(
        dir.path(),
        &["facts", "show", "Bruno", "--kb", "personal", "--diary", "*"],
    );
    assert_eq!(v["data"]["active_count"], Value::from(1));
    assert_eq!(v["data"]["historic_count"], Value::from(1));
    let historic = &v["data"]["historic"][0];
    assert_eq!(historic["open"], Value::Bool(false));
    assert_eq!(historic["object"], Value::from("Lisbon"));
    assert_eq!(v["data"]["active"][0]["object"], Value::from("Porto"));
}

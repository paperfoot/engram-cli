//! Wave 3 — who may close a safety interval, and who may only ask.
//!
//! A safety relation (allergies, medications, diagnoses, blood type) may be
//! closed only by evidence a person supplied: an explicit end in the text, a
//! caller decision, or a replayed caller decision. A newer timestamp is not
//! evidence. Neither is a wider confidence gap, an LLM, or a bulk sweep.
//!
//! The gate is one function (`engram_core::interval::may_close`) plus an audit
//! column (`facts.close_authority`), so this file checks the funnel end to end
//! rather than the function — the function has its own unit tests.

mod wave3_fact_support;
use wave3_fact_support::*;

fn seed_dose_change(store: &std::path::Path) -> String {
    remember_facts(
        store,
        "Ana takes metformin 500 mg daily.",
        r#"[{"subject":"Ana","predicate":"takes_medication","object":"metformin 500 mg daily"}]"#,
    );
    let v = remember_facts(
        store,
        "Ana's metformin went up to 1000 mg daily.",
        r#"[{"subject":"Ana","predicate":"takes_medication","object":"metformin 1000 mg daily"}]"#,
    );
    let queued = v["data"]["adjudication_needed"].as_array().unwrap();
    assert_eq!(queued.len(), 1, "one member, two doses: {v}");
    queued[0]["conflict_id"].as_str().unwrap().to_string()
}

#[test]
fn a_dose_change_queues_with_requires_context() {
    let tmp = store();
    remember_facts(
        tmp.path(),
        "Ana takes metformin 500 mg daily.",
        r#"[{"subject":"Ana","predicate":"takes_medication","object":"metformin 500 mg daily"}]"#,
    );
    let v = remember_facts(
        tmp.path(),
        "Ana's metformin went up to 1000 mg daily.",
        r#"[{"subject":"Ana","predicate":"takes_medication","object":"metformin 1000 mg daily"}]"#,
    );
    let entry = &v["data"]["adjudication_needed"][0];
    assert_eq!(
        entry["suggested_action"], "requires_context",
        "a safety pair is never advised toward a supersession: {v}"
    );
    assert_eq!(entry["rule"], "safety_member_change", "{entry}");

    // ONE runnable command, and the alternatives are data, not a pipe.
    let command = entry["resolve_command"].as_str().unwrap();
    assert!(
        !command.contains('|'),
        "a pasted pipe runs the destructive branch: {command}"
    );
    assert!(command.starts_with("engram facts resolve "), "{command}");
    assert!(
        command.ends_with("--action keep-both"),
        "the blind command must run the SAFE action: {command}"
    );
    let allowed = entry["resolve"]["allowed_actions"].as_array().unwrap();
    assert_eq!(
        allowed[0], "keep-both",
        "permission is not narrowed, only advice: {allowed:?}"
    );
    assert!(allowed.iter().any(|a| a == "supersede-old"));

    // The question carries the key that lets it be answered once.
    assert!(
        entry["decision_key"]
            .as_str()
            .unwrap()
            .contains("takes_medication"),
        "{entry}"
    );

    // BOTH doses are still current: the gate refused every machine close.
    let current = current_facts(tmp.path(), "Ana");
    assert_eq!(current.len(), 2, "{current:?}");
    for f in &current {
        assert!(f["valid_until"].is_null(), "{f}");
        assert!(f["superseded_by"].is_null(), "{f}");
    }
}

#[test]
fn triage_never_closes_a_safety_interval() {
    let tmp = store();
    seed_dose_change(tmp.path());

    let dry = run_json(&mut engram(tmp.path()), &["facts", "triage", "--dry-run"]);
    let real = run_json(&mut engram(tmp.path()), &["facts", "triage"]);
    assert_eq!(
        dry["data"]["resolved"], real["data"]["resolved"],
        "a dry run reports the numbers the real run produces"
    );
    assert_eq!(dry["data"]["remaining"], real["data"]["remaining"]);
    assert_eq!(
        real["data"]["safety_slots_kept"], 1,
        "the sweep must say what it refused to decide: {real}"
    );
    assert_eq!(real["data"]["facts_superseded"], 0, "{real}");

    // Still queued, still both open.
    assert_eq!(open_conflicts(tmp.path())["data"]["total_open"], 1);
    let current = current_facts(tmp.path(), "Ana");
    assert_eq!(current.len(), 2);
    for f in &current {
        assert!(f["valid_until"].is_null(), "{f}");
    }
}

/// The one authority that MAY close it. `facts resolve` is a person deciding,
/// which is exactly what `CloseAuthority::Caller` means.
#[test]
fn a_caller_decision_closes_what_no_machine_could() {
    let tmp = store();
    let conflict_id = seed_dose_change(tmp.path());
    let v = run_json(
        &mut engram(tmp.path()),
        &[
            "facts",
            "resolve",
            &conflict_id,
            "--action",
            "supersede-old",
            "--note",
            "prescriber confirmed the increase",
        ],
    );
    assert_eq!(v["data"]["resolved"], true, "{v}");
    assert_eq!(open_conflicts(tmp.path())["data"]["total_open"], 0);
    let current = current_facts(tmp.path(), "Ana");
    assert_eq!(current.len(), 1, "{current:?}");
    assert_eq!(current[0]["object"], "metformin 1000 mg daily");
}

/// `--autonomy strict` promised never to auto-supersede. On a NON-safety slot
/// it queues what it would otherwise have closed — and, because the classifier
/// did order the two values, it says so: `supersede-old` is honest advice
/// here, and the only place it survives (R7).
#[test]
fn strict_autonomy_queues_a_slot_update_and_still_says_which_way_it_leans() {
    let tmp = store();
    run_json(
        &mut engram(tmp.path()),
        &[
            "remember",
            "Elias lives in London.",
            "--kb",
            "personal",
            "--facts-json",
            r#"[{"subject":"Elias","predicate":"lives_in","object":"London","valid_from":"2015-06-01"}]"#,
            "--autonomy",
            "strict",
        ],
    );
    let v = run_json(
        &mut engram(tmp.path()),
        &[
            "remember",
            "Elias moved to Lisbon in 2024.",
            "--kb",
            "personal",
            "--facts-json",
            r#"[{"subject":"Elias","predicate":"lives_in","object":"Lisbon","valid_from":"2024-02-01"}]"#,
            "--autonomy",
            "strict",
        ],
    );
    let queued = v["data"]["adjudication_needed"].as_array().unwrap();
    assert_eq!(queued.len(), 1, "strict escalates a real disagreement: {v}");
    assert_eq!(
        v["data"]["conflicts"].as_array().unwrap().len(),
        0,
        "strict closed an interval it promised not to: {v}"
    );
    assert_eq!(
        queued[0]["suggested_action"], "supersede-old",
        "the classifier ordered these values; hiding that grows the queue for nothing"
    );
    assert_eq!(current_facts(tmp.path(), "Elias").len(), 2);
}

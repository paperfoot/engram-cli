//! Wave 3 — the storm, end to end.
//!
//! Five medications on one person used to produce ten queued "conflicts",
//! because `takes_medication` was derived to be a single-valued slot. This
//! file writes the five through the real CLI and asserts the queue stays
//! empty, every interval stays open, and a triage sweep finds nothing to do.
//!
//! The unit tests in `engram_graph::semantics` prove the same property over
//! fixtures; this one proves the wiring, which is the half a pure test cannot
//! see.

mod wave3_fact_support;
use wave3_fact_support::*;

const MEDS: [&str; 5] = [
    "empagliflozin 12.5 mg daily",
    "rapamycin 6 mg weekly",
    "metformin 500 mg twice daily",
    "lisinopril 10 mg daily",
    "levothyroxine 75 mcg daily",
];

#[test]
fn five_medications_queue_nothing() {
    let tmp = store();
    for med in MEDS {
        let v = remember_facts(
            tmp.path(),
            &format!("Ana takes {med}."),
            &format!(r#"[{{"subject":"Ana","predicate":"takes_medication","object":"{med}"}}]"#),
        );
        assert_eq!(
            v["data"]["adjudication_needed"].as_array().unwrap().len(),
            0,
            "{med} queued a question: {v}"
        );
        assert_eq!(
            v["data"]["conflicts"].as_array().unwrap().len(),
            0,
            "{med} superseded something: {v}"
        );
    }

    let open = open_conflicts(tmp.path());
    assert_eq!(open["data"]["count"], 0, "{open}");
    assert_eq!(open["data"]["total_open"], 0, "{open}");

    let facts = facts_for(tmp.path(), "Ana");
    assert_eq!(facts.len(), 5, "{facts:?}");
    for f in &facts {
        assert!(
            f["valid_until"].is_null(),
            "every medication is still current: {f}"
        );
        assert!(f["superseded_by"].is_null(), "{f}");
    }

    // Nothing for the sweep to find, because nothing was ever queued.
    let triage = run_json(&mut engram(tmp.path()), &["facts", "triage"]);
    assert_eq!(triage["data"]["scanned"], 0, "{triage}");
    assert_eq!(triage["data"]["remaining"], 0, "{triage}");
}

/// The same five, asserted in ONE batch out of one memory. Same-source pairs
/// are the case where recency proves nothing, so this is where a slot-shaped
/// arity would have queued hardest.
#[test]
fn five_medications_from_one_memory_queue_nothing() {
    let tmp = store();
    let triples: Vec<String> = MEDS
        .iter()
        .map(|m| format!(r#"{{"subject":"Ana","predicate":"takes_medication","object":"{m}"}}"#))
        .collect();
    let v = remember_facts(
        tmp.path(),
        "Ana's current regimen: empagliflozin, rapamycin, metformin, lisinopril, levothyroxine.",
        &format!("[{}]", triples.join(",")),
    );
    assert_eq!(v["data"]["facts_added"], 5, "{v}");
    assert_eq!(
        v["data"]["adjudication_needed"].as_array().unwrap().len(),
        0,
        "{v}"
    );
    assert_eq!(open_conflicts(tmp.path())["data"]["total_open"], 0);
}

/// A re-assertion is a confirmation, not a question — including on a safety
/// relation, where turning "still allergic to penicillin" into an adjudication
/// would be the storm wearing a different hat.
#[test]
fn a_re_asserted_allergy_is_confirmed_not_queued() {
    let tmp = store();
    let facts = r#"[{"subject":"Ana","predicate":"allergic_to","object":"penicillin"}]"#;
    remember_facts(tmp.path(), "Ana is allergic to penicillin.", facts);
    let v = remember_facts(tmp.path(), "Ana is allergic to penicillin.", facts);
    assert_eq!(v["data"]["facts_added"], 0, "{v}");
    assert_eq!(v["data"]["facts_deduped"], 1, "{v}");
    assert_eq!(
        v["data"]["adjudication_needed"].as_array().unwrap().len(),
        0,
        "{v}"
    );
    let facts = facts_for(tmp.path(), "Ana");
    assert_eq!(facts.len(), 1);
    assert_eq!(facts[0]["confirmations"], 1);
    assert!(
        facts[0]["superseded_by"].is_null(),
        "a confirmation must not retire the row it confirms: {}",
        facts[0]
    );
}

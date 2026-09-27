//! Grounding the facts engram's own extractor writes (`remember`,
//! `facts backfill`) against the memory they came from.
//!
//! The extractor invents facts: "Sam allergic_to ibuprofen" from "avoid
//! ibuprofen with the kidney issue" (judge p 0.04 on the remember eval). With
//! `judge.grounding = on` an extracted SAFETY-set fact is written only when
//! the judge admits it; everything else is annotated. A caller's fact is never
//! dropped. The negated-object rule runs in every mode, with no judge.
//!
//! Offline: extraction is `ENGRAM_FACT_LLM_STUB`, the judge is
//! `ENGRAM_JUDGE_STUB` (rules keyed by substrings of the rendered claim), and
//! the harness strips every key.

mod wave3_fact_support;

use serde_json::Value;
use std::collections::BTreeSet;
use wave3_fact_support::*;

const KIDNEY: &str =
    "Sam should avoid ibuprofen with the kidney issue. Sam is allergic to penicillin.";
const KIDNEY_FACTS: &str = r#"[
    {"subject":"Sam","predicate":"allergic_to","object":"ibuprofen"},
    {"subject":"Sam","predicate":"allergic_to","object":"penicillin"}
]"#;
const KIDNEY_JUDGE: &str = r#"{"allergic to ibuprofen": 0.04, "allergic to penicillin": 0.97}"#;

/// `remember` with a scripted extraction and, when `judge` is given, a
/// scripted grounding judge in `mode`.
fn remember_extracted(
    store: &std::path::Path,
    content: &str,
    extraction: &str,
    judge: Option<(&str, &str)>,
) -> Value {
    let mut cmd = engram(store);
    cmd.env("ENGRAM_FACT_LLM_STUB", extraction);
    if let Some((mode, spec)) = judge {
        cmd.env("ENGRAM_JUDGE_GROUNDING", mode);
        cmd.env("ENGRAM_JUDGE_STUB", spec);
    }
    run_json(&mut cmd, &["remember", content, "--kb", "personal"])
}

fn grounding(v: &Value) -> Vec<Value> {
    v["data"]["facts_grounding"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

fn entry_for<'a>(entries: &'a [Value], object: &str) -> &'a Value {
    entries
        .iter()
        .find(|e| e["object"] == object)
        .unwrap_or_else(|| panic!("no grounding entry for {object}: {entries:?}"))
}

fn objects(store: &std::path::Path, subject: &str) -> BTreeSet<String> {
    facts_for(store, subject)
        .iter()
        .map(|f| {
            format!(
                "{} {}",
                f["predicate"].as_str().unwrap_or_default(),
                f["object"].as_str().unwrap_or_default()
            )
        })
        .collect()
}

fn p_of(entry: &Value) -> f64 {
    entry["p"].as_f64().expect("a judged entry carries p")
}

#[test]
fn on_drops_an_unsupported_extracted_allergy_and_keeps_a_supported_one() {
    let tmp = store();
    let v = remember_extracted(tmp.path(), KIDNEY, KIDNEY_FACTS, Some(("on", KIDNEY_JUDGE)));
    assert_eq!(v["data"]["facts_added"], 1, "{v}");
    let entries = grounding(&v);
    assert_eq!(entries.len(), 2, "{v}");

    let ibuprofen = entry_for(&entries, "ibuprofen");
    assert_eq!(ibuprofen["action"], "dropped");
    assert_eq!(ibuprofen["verdict"], "unsupported");
    assert_eq!(ibuprofen["reason"], "unsupported_safety_fact");
    assert_eq!(ibuprofen["source_kind"], "agent_extracted");
    assert_eq!(ibuprofen["subject"], "Sam");
    assert_eq!(ibuprofen["predicate"], "allergic_to");
    assert!((p_of(ibuprofen) - 0.04).abs() < 1e-6);

    let penicillin = entry_for(&entries, "penicillin");
    assert_eq!(penicillin["action"], "written");
    assert_eq!(penicillin["verdict"], "supported");

    assert_eq!(
        objects(tmp.path(), "Sam"),
        BTreeSet::from(["allergic_to penicillin".to_string()])
    );
    let counts = &v["metadata"]["grounding"];
    assert_eq!(counts["judged"], 2, "{v}");
    assert_eq!(counts["dropped_by_judge"], 1, "{v}");
    assert_eq!(counts["judge_failed"], 0, "{v}");
}

#[test]
fn on_keeps_a_non_safety_extracted_fact_the_judge_doubts_and_annotates_it() {
    let tmp = store();
    let v = remember_extracted(
        tmp.path(),
        "Sam had green tea at the meeting.",
        r#"[{"subject":"Sam","predicate":"prefers","object":"green tea"}]"#,
        Some(("on", r#"{"*": 0.1}"#)),
    );
    assert_eq!(v["data"]["facts_added"], 1, "{v}");
    let entry = entry_for(&grounding(&v), "green tea").clone();
    assert_eq!(entry["action"], "written");
    assert_eq!(entry["verdict"], "unsupported");
    assert_eq!(entry["reason"], "not_a_safety_relation");
    assert!((p_of(&entry) - 0.1).abs() < 1e-6);
}

#[test]
fn a_caller_fact_is_never_dropped_whatever_the_judge_says() {
    let tmp = store();
    let v = run_json(
        engram(tmp.path())
            .env("ENGRAM_JUDGE_GROUNDING", "on")
            .env("ENGRAM_JUDGE_STUB", r#"{"*": 0.01}"#),
        &[
            "remember",
            "Sam mentioned the latex gloves at the clinic.",
            "--kb",
            "personal",
            "--facts-json",
            r#"[{"subject":"Sam","predicate":"allergic_to","object":"latex"}]"#,
        ],
    );
    assert_eq!(v["data"]["facts_added"], 1, "{v}");
    let entry = entry_for(&grounding(&v), "latex").clone();
    assert_eq!(entry["source_kind"], "user_stated");
    assert_eq!(entry["verdict"], "unsupported");
    assert_eq!(entry["action"], "written");
    assert_eq!(entry["reason"], "caller_asserted");
    assert_eq!(
        objects(tmp.path(), "Sam"),
        BTreeSet::from(["allergic_to latex".to_string()])
    );
}

#[test]
fn an_ended_fact_is_judged_as_a_cessation() {
    let tmp = store();
    // Only the "no longer" rendering scores high: a cessation judged as the
    // ongoing claim would score 0.05 and be dropped.
    let v = remember_extracted(
        tmp.path(),
        "Ana stopped taking metformin in June.",
        r#"[{"subject":"Ana","predicate":"takes_medication","object":"metformin","state":"ended"}]"#,
        Some((
            "on",
            r#"{"Ana no longer takes medication metformin": 0.9, "*": 0.05}"#,
        )),
    );
    let entry = entry_for(&grounding(&v), "metformin").clone();
    assert_eq!(entry["claim"], "Ana no longer takes medication metformin");
    assert_eq!(entry["action"], "written", "{v}");
    assert!((p_of(&entry) - 0.9).abs() < 1e-6);
}

#[test]
fn a_negated_medication_is_dropped_with_the_judge_off_but_never_a_callers() {
    let tmp = store();
    let v = remember_extracted(
        tmp.path(),
        "Sam takes metformin. No blood thinners.",
        r#"[{"subject":"Sam","predicate":"takes_medication","object":"no blood thinners"},
            {"subject":"Sam","predicate":"takes_medication","object":"metformin"}]"#,
        None,
    );
    assert_eq!(v["data"]["facts_added"], 1, "{v}");
    let entries = grounding(&v);
    assert_eq!(entries.len(), 1, "off lists only the dropped fact: {v}");
    let entry = &entries[0];
    assert_eq!(entry["object"], "no blood thinners");
    assert_eq!(entry["index"], 0);
    assert_eq!(entry["action"], "dropped");
    assert_eq!(entry["reason"], "negated_object");
    assert_eq!(entry["verdict"], "not_judged");
    assert!(entry["p"].is_null());
    assert_eq!(v["metadata"]["grounding"]["dropped_negated_object"], 1);
    assert_eq!(
        objects(tmp.path(), "Sam"),
        BTreeSet::from(["takes_medication metformin".to_string()])
    );

    // The same triple from the caller is the caller's call.
    let other = store();
    let v = remember_facts(
        other.path(),
        "Sam takes no blood thinners.",
        r#"[{"subject":"Sam","predicate":"takes_medication","object":"no blood thinners"}]"#,
    );
    assert_eq!(v["data"]["facts_added"], 1, "{v}");
    assert!(v["data"].get("facts_grounding").is_none(), "{v}");
}

#[test]
fn an_invented_number_drops_an_extracted_safety_fact_the_judge_admits() {
    let tmp = store();
    let v = remember_extracted(
        tmp.path(),
        "Sam takes metformin 500 mg twice a day.",
        r#"[{"subject":"Sam","predicate":"takes_medication","object":"metformin 1000 mg"}]"#,
        Some(("on", r#"{"*": 0.9}"#)),
    );
    assert_eq!(v["data"]["facts_added"], 0, "{v}");
    let entry = entry_for(&grounding(&v), "metformin 1000 mg").clone();
    assert_eq!(entry["action"], "dropped");
    assert_eq!(entry["reason"], "number_not_in_text");
}

#[test]
fn a_judge_that_does_not_answer_writes_as_before_and_says_so() {
    let tmp = store();
    // No rule matches and there is no "*": the stub leaves every question
    // unanswered, as a provider that dropped them would.
    let v = remember_extracted(
        tmp.path(),
        KIDNEY,
        KIDNEY_FACTS,
        Some(("on", r#"{"nothing matches this": 0.9}"#)),
    );
    assert_eq!(v["data"]["facts_added"], 2, "{v}");
    for entry in grounding(&v) {
        assert_eq!(entry["action"], "written", "{entry}");
        assert_eq!(entry["verdict"], "not_judged", "{entry}");
        assert_eq!(entry["reason"], "judge_failed", "{entry}");
    }
    assert_eq!(v["metadata"]["grounding"]["judge_failed"], 2, "{v}");
    assert_eq!(v["metadata"]["grounding"]["judged"], 0, "{v}");
}

#[test]
fn shadow_writes_exactly_what_off_writes() {
    let extraction = r#"[
        {"subject":"Sam","predicate":"allergic_to","object":"ibuprofen"},
        {"subject":"Sam","predicate":"allergic_to","object":"penicillin"},
        {"subject":"Sam","predicate":"takes_medication","object":"none"}
    ]"#;
    let off = store();
    let v_off = remember_extracted(off.path(), KIDNEY, extraction, None);
    let shadow = store();
    let v_shadow = remember_extracted(
        shadow.path(),
        KIDNEY,
        extraction,
        Some(("shadow", KIDNEY_JUDGE)),
    );
    assert_eq!(v_off["data"]["facts_added"], 2, "{v_off}");
    assert_eq!(v_shadow["data"]["facts_added"], 2, "{v_shadow}");
    assert_eq!(objects(off.path(), "Sam"), objects(shadow.path(), "Sam"));

    let entries = grounding(&v_shadow);
    let ibuprofen = entry_for(&entries, "ibuprofen");
    assert_eq!(ibuprofen["action"], "written");
    assert_eq!(ibuprofen["verdict"], "unsupported");
    assert_eq!(ibuprofen["reason"], "shadow");
    assert_eq!(entry_for(&entries, "none")["reason"], "negated_object");
    assert_eq!(v_shadow["metadata"]["grounding"]["dropped_by_judge"], 0);
}

#[test]
fn five_medications_are_unchanged_under_on() {
    let tmp = store();
    let v = remember_extracted(
        tmp.path(),
        "Ana takes metformin, lisinopril, atorvastatin, levothyroxine and omeprazole.",
        r#"[{"subject":"Ana","predicate":"takes_medication","object":"metformin"},
            {"subject":"Ana","predicate":"takes_medication","object":"lisinopril"},
            {"subject":"Ana","predicate":"takes_medication","object":"atorvastatin"},
            {"subject":"Ana","predicate":"takes_medication","object":"levothyroxine"},
            {"subject":"Ana","predicate":"takes_medication","object":"omeprazole"}]"#,
        Some(("on", r#"{"*": 0.95}"#)),
    );
    assert_eq!(v["data"]["facts_added"], 5, "{v}");
    assert_eq!(
        v["data"]["adjudication_needed"],
        serde_json::json!([]),
        "{v}"
    );
    assert_eq!(v["metadata"]["grounding"]["judged"], 5, "{v}");
    assert_eq!(v["metadata"]["grounding"]["dropped_by_judge"], 0, "{v}");
    assert_eq!(objects(tmp.path(), "Ana").len(), 5);
}

#[test]
fn backfill_grounds_what_it_extracts() {
    let tmp = store();
    run_json(
        &mut engram(tmp.path()),
        &["remember", KIDNEY, "--kb", "personal", "--no-extract-facts"],
    );
    let v = run_json(
        engram(tmp.path())
            .env("ENGRAM_FACT_LLM_STUB", KIDNEY_FACTS)
            .env("ENGRAM_JUDGE_GROUNDING", "on")
            .env("ENGRAM_JUDGE_STUB", KIDNEY_JUDGE),
        &["facts", "backfill", "--kb", "personal"],
    );
    assert_eq!(v["data"]["facts_added"], 1, "{v}");
    let entries = grounding(&v);
    assert_eq!(entry_for(&entries, "ibuprofen")["action"], "dropped");
    assert!(entry_for(&entries, "ibuprofen")["memory_id"].is_string());
    assert_eq!(v["metadata"]["grounding"]["dropped_by_judge"], 1, "{v}");
    assert_eq!(
        objects(tmp.path(), "Sam"),
        BTreeSet::from(["allergic_to penicillin".to_string()])
    );
}

#[test]
fn on_with_no_reachable_judge_behaves_as_off() {
    // The harness forces the stub embedder (ENGRAM_BENCH_FORCE_STUB) and
    // strips every key: no judge route, so no judge, no annotation.
    let tmp = store();
    let mut cmd = engram(tmp.path());
    cmd.env("ENGRAM_FACT_LLM_STUB", KIDNEY_FACTS)
        .env("ENGRAM_JUDGE_GROUNDING", "on");
    let v = run_json(&mut cmd, &["remember", KIDNEY, "--kb", "personal"]);
    assert_eq!(v["data"]["facts_added"], 2, "{v}");
    assert!(v["data"].get("facts_grounding").is_none(), "{v}");
}

#[test]
fn a_bad_grounding_mode_is_a_config_error_before_anything_is_stored() {
    let tmp = store();
    engram(tmp.path())
        .env("ENGRAM_JUDGE_GROUNDING", "yes")
        .args(["remember", KIDNEY, "--kb", "personal", "--json"])
        .assert()
        .code(2);
    let pending = run_json(
        &mut engram(tmp.path()),
        &["facts", "backfill", "--kb", "personal", "--dry-run"],
    );
    assert_eq!(pending["data"]["memories_total"], 0, "{pending}");
}

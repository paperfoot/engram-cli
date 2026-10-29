//! Wave 5 — `engram state <subject>`: what is TRUE NOW, and how it knows.
//!
//! The defect these tests close: `facts show ada` lists a medication she
//! stopped and one she takes today identically, because closing an interval
//! with reason `stopped` leaves `superseded_by` NULL. Test 1 is the regression
//! for exactly that, and it fails the moment `state` ever tests
//! `superseded_by IS NULL` itself.

mod state_support;

use serde_json::{json, Value};
use state_support::*;

fn med(subject: &str, object: &str, from: &str) -> String {
    format!(
        r#"[{{"subject":"{subject}","predicate":"takes_medication","object":"{object}",
             "valid_from":"{from}","confidence":0.9}}]"#
    )
}

#[test]
fn only_open_intervals_are_current() {
    let dir = store();
    remember_facts(
        dir.path(),
        "personal",
        "Ada started empagliflozin in March 2024.",
        &med("Ada Example", "empagliflozin", "2024-03-01"),
    );
    remember_facts(
        dir.path(),
        "personal",
        "Ada started metformin in January 2024.",
        &med("Ada Example", "metformin", "2024-01-01"),
    );
    remember_facts(
        dir.path(),
        "personal",
        "Ada stopped metformin in June 2025.",
        r#"[{"subject":"Ada Example","predicate":"stopped_taking","object":"metformin",
             "valid_until":"2025-06-01"}]"#,
    );

    let v = state(dir.path(), "Ada Example", &[]);
    let values = group_values(&v, "takes_medication");
    assert_eq!(values.len(), 1, "a stopped medication is not current: {v}");
    assert_eq!(values[0]["object"], json!("empagliflozin"));
    assert_eq!(v["data"]["verdict"], json!("results"));
}

#[test]
fn coexisting_medications_are_not_a_conflict() {
    let dir = store();
    for (i, drug) in [
        "metformin",
        "empagliflozin",
        "atorvastatin",
        "ramipril",
        "aspirin",
    ]
    .iter()
    .enumerate()
    {
        remember_facts(
            dir.path(),
            "personal",
            &format!("Ada takes {drug}."),
            &med("Ada Example", drug, &format!("2024-0{}-01", i + 1)),
        );
    }
    let v = state(dir.path(), "Ada Example", &[]);
    assert_eq!(group_values(&v, "takes_medication").len(), 5);
    assert_eq!(
        v["data"]["open_questions"].as_array().unwrap().len(),
        0,
        "five medications are five facts, not ten questions: {v}"
    );
}

#[test]
fn the_registry_fields_come_from_the_registry() {
    let dir = store();
    remember_facts(
        dir.path(),
        "personal",
        "Ada takes metformin.",
        &med("Ada Example", "metformin", "2024-01-01"),
    );
    remember_facts(
        dir.path(),
        "personal",
        "Ada is O negative.",
        r#"[{"subject":"Ada Example","predicate":"blood_type","object":"O negative"}]"#,
    );
    remember_facts(
        dir.path(),
        "personal",
        "Ada was born on 2 April 1988.",
        r#"[{"subject":"Ada Example","predicate":"birthday","object":"1988-04-02",
             "valid_from":"1988-04-02"}]"#,
    );

    let v = state(dir.path(), "Ada Example", &[]);
    let groups = v["data"]["state"].as_array().cloned().unwrap_or_default();
    let by = |r: &str| -> Value {
        groups
            .iter()
            .find(|g| g["relation"] == r)
            .cloned()
            .unwrap_or_else(|| panic!("no group {r} in {v}"))
    };
    let meds = by("takes_medication");
    assert_eq!(meds["arity_class"], json!("set_valued"));
    assert_eq!(meds["safety"], json!(true));
    assert_eq!(meds["mutability"], json!("mutable"));
    assert_eq!(meds["temporality"], json!("state"));

    let blood = by("blood_type");
    assert_eq!(blood["arity_class"], json!("single_valued"));
    assert_eq!(blood["safety"], json!(true));
    assert_eq!(blood["mutability"], json!("immutable"));

    let bday = by("birthday");
    assert_eq!(bday["safety"], json!(false));
    assert_eq!(bday["temporality"], json!("instant"));
    assert_eq!(bday["value_domain"], json!("date"));

    // Safety slots sort first: a caller scanning the top of the output sees
    // the slots that can hurt someone.
    assert!(groups[0]["safety"] == json!(true), "safety first: {v}");
}

#[test]
fn two_runs_on_one_store_are_byte_identical() {
    let dir = store();
    for drug in ["metformin", "empagliflozin"] {
        remember_facts(
            dir.path(),
            "personal",
            &format!("Ada takes {drug}."),
            &med("Ada Example", drug, "2024-01-01"),
        );
    }
    let a = state(dir.path(), "Ada Example", &[]);
    let b = state(dir.path(), "Ada Example", &[]);
    // `as_of` defaults to now and legitimately differs; everything else must
    // not. Two answers to one question is the defect, not the clock.
    let strip = |mut v: Value| {
        v["data"]["as_of"] = Value::Null;
        v["data"].clone()
    };
    assert_eq!(strip(a), strip(b));
}

#[test]
fn every_value_carries_its_provenance_chain() {
    let dir = store();
    remember_facts(
        dir.path(),
        "personal",
        "Ada takes metformin, 500mg, twice daily.",
        &med("Ada Example", "metformin", "2024-01-01"),
    );
    let v = state(dir.path(), "Ada Example", &[]);
    let value = &group_values(&v, "takes_medication")[0];
    let prov = &value["provenance"];
    let memory = prov["memory"].as_str().expect("memory handle");
    assert!(memory.starts_with("mem:"), "{prov}");
    assert!(
        prov["chunk"].as_str().unwrap().starts_with("chunk:"),
        "{prov}"
    );
    assert!(prov["observed_at"].is_string(), "{prov}");
    assert!(
        !prov["snippet"].as_str().unwrap_or("").is_empty(),
        "an empty snippet proves nothing: {prov}"
    );
    // Keys are present even when null — an agent branching on "no document"
    // must not have to tell it apart from "field not implemented".
    assert!(prov.get("document_id").is_some(), "{prov}");

    // The id it publishes is one a mutator accepts.
    let raw = prov["memory_id"].as_str().expect("memory_id").to_string();
    let edited = run_json(dir.path(), &["edit", &raw, "--importance", "6"]);
    assert_eq!(edited["data"]["id"], json!(raw));
}

#[test]
fn confidence_equals_the_sum_of_its_published_basis() {
    let dir = store();
    remember_facts(
        dir.path(),
        "personal",
        "Ada takes metformin.",
        &med("Ada Example", "metformin", "2024-01-01"),
    );
    let v = state(dir.path(), "Ada Example", &[]);
    let value = &group_values(&v, "takes_medication")[0];
    let basis = &value["confidence_basis"];
    let sum: f64 = [
        "extraction",
        "source_kind",
        "confirmations",
        "staleness",
        "contested",
    ]
    .iter()
    .map(|k| basis[*k].as_f64().unwrap_or(0.0))
    .sum();
    let confidence = value["confidence"].as_f64().expect("confidence");
    assert!(
        (confidence - sum.clamp(0.01, 0.99)).abs() < 1e-4,
        "confidence {confidence} is not its stated basis {basis}"
    );
    // A caller-asserted triple is `user_stated` (R13), and the basis says so
    // rather than hiding the bonus inside one opaque number.
    assert_eq!(value["source_kind"], json!("user_stated"));
    assert!(basis["source_kind"].as_f64().unwrap() > 0.0);
    // Staleness is negative for a mutable slot with an old valid_from.
    assert!(basis["staleness"].as_f64().unwrap() < 0.0, "{basis}");
    assert!(!basis["contested"].is_null(), "we looked, so we say so");
}

#[test]
fn an_immutable_slot_never_goes_stale() {
    let dir = store();
    remember_facts(
        dir.path(),
        "personal",
        "Ada is O negative.",
        r#"[{"subject":"Ada Example","predicate":"blood_type","object":"O negative",
             "valid_from":"2011-01-01"}]"#,
    );
    let v = state(dir.path(), "Ada Example", &[]);
    let value = &group_values(&v, "blood_type")[0];
    assert_eq!(
        value["confidence_basis"]["staleness"].as_f64().unwrap(),
        0.0,
        "a blood type learned in 2011 is as true today: {value}"
    );
}

#[test]
fn a_contested_value_is_marked_and_the_question_is_attached() {
    let dir = store();
    remember_facts(
        dir.path(),
        "personal",
        "Ada lives in Lisbon.",
        r#"[{"subject":"Ada Example","predicate":"lives_in","object":"Lisbon"}]"#,
    );
    run_json(
        dir.path(),
        &[
            "remember",
            "Ada lives in Porto.",
            "--kb",
            "personal",
            "--offline",
            "--autonomy",
            "strict",
            "--facts-json",
            r#"[{"subject":"Ada Example","predicate":"lives_in","object":"Porto"}]"#,
        ],
    );

    let v = state(dir.path(), "Ada Example", &[]);
    let questions = v["data"]["open_questions"].as_array().cloned().unwrap();
    assert_eq!(questions.len(), 1, "one unresolved contradiction: {v}");
    let q = &questions[0];
    assert!(q["handle"].as_str().unwrap().starts_with("conflict:"));
    assert_eq!(q["relation"], json!("lives_in"));
    assert!(
        q["resolve_command"]
            .as_str()
            .unwrap()
            .contains("facts resolve"),
        "an open question must be actionable: {q}"
    );
    // And the group carries the same question, so a caller reading one
    // relation does not have to cross-reference the flat list.
    let group = v["data"]["state"]
        .as_array()
        .unwrap()
        .iter()
        .find(|g| g["relation"] == "lives_in")
        .cloned()
        .unwrap();
    assert_eq!(group["open_questions"].as_array().unwrap().len(), 1);
    let contested = group_values(&v, "lives_in").iter().any(|value| {
        value["confidence_basis"]["contested"]
            .as_f64()
            .is_some_and(|c| (c + 0.15).abs() < 1e-6)
    });
    assert!(
        contested,
        "a contested side must pay the contested term: {v}"
    );
}

#[test]
fn totals_are_not_page_sizes_and_the_cursor_pages_without_gaps() {
    let dir = store();
    for i in 0..30 {
        remember_facts(
            dir.path(),
            "personal",
            &format!("Ada takes drug{i}."),
            &med("Ada Example", &format!("drug{i:02}"), "2024-01-01"),
        );
    }
    let first = state(dir.path(), "Ada Example", &["--limit", "10"]);
    assert_eq!(first["data"]["returned"], json!(10));
    assert_eq!(first["data"]["total"], json!(30));
    let cursor = first["data"]["cursor"]
        .as_str()
        .expect("a cursor")
        .to_string();

    let second = state(
        dir.path(),
        "Ada Example",
        &["--limit", "10", "--cursor", &cursor],
    );
    assert_eq!(second["data"]["returned"], json!(10));
    let names = |v: &Value| -> Vec<String> {
        group_values(v, "takes_medication")
            .iter()
            .map(|x| x["object"].as_str().unwrap_or("").to_string())
            .collect()
    };
    let (a, b) = (names(&first), names(&second));
    assert!(
        a.iter().all(|x| !b.contains(x)),
        "pages overlap: {a:?} {b:?}"
    );

    // A cursor from a different scope is exit 3, never a plausible page of the
    // wrong rows.
    let (code, err) = run_err(
        dir.path(),
        &[
            "state",
            "Ada Example",
            "--kb",
            "personal",
            "--cursor",
            &cursor,
        ],
    );
    assert_eq!(code, 3, "{err}");
}

#[test]
fn paging_never_hides_a_relation() {
    // The v0.5-agenda defect class: with one allergy and two medications,
    // `--limit 1` used to build groups from the PAGE, so the medication group
    // vanished entirely and an agent reading `state[]` concluded "no
    // medications known" — about a safety slot. Every open relation must
    // appear on every page; only the values inside it are paged.
    let dir = store();
    remember_facts(
        dir.path(),
        "personal",
        "Ada is allergic to penicillin.",
        r#"[{"subject":"Ada Example","predicate":"allergic_to","object":"penicillin",
             "valid_from":"2024-01-01","confidence":0.9}]"#,
    );
    remember_facts(
        dir.path(),
        "personal",
        "Ada takes empagliflozin.",
        &med("Ada Example", "empagliflozin", "2024-03-01"),
    );
    remember_facts(
        dir.path(),
        "personal",
        "Ada takes metformin.",
        &med("Ada Example", "metformin", "2024-01-01"),
    );

    let v = state(dir.path(), "Ada Example", &["--limit", "1"]);
    assert_eq!(v["data"]["total"], json!(3));
    assert_eq!(v["data"]["returned"], json!(1));
    let groups = v["data"]["state"].as_array().expect("groups");
    let relations: Vec<&str> = groups
        .iter()
        .map(|g| g["relation"].as_str().unwrap_or(""))
        .collect();
    assert_eq!(
        relations,
        vec!["allergic_to", "takes_medication"],
        "a relation whose facts fell past the limit must still appear: {v}"
    );
    let meds = &groups[1];
    assert_eq!(
        meds["open_count"],
        json!(2),
        "open_count is the true count, not the page size: {v}"
    );
    assert_eq!(
        meds["values"].as_array().map(|a| a.len()),
        Some(0),
        "values page out; the group does not: {v}"
    );
    assert_eq!(v["data"]["verdict"], json!("results"));
}

#[test]
fn open_count_survives_paging_within_one_relation() {
    let dir = store();
    remember_facts(
        dir.path(),
        "personal",
        "Ada takes empagliflozin.",
        &med("Ada Example", "empagliflozin", "2024-03-01"),
    );
    remember_facts(
        dir.path(),
        "personal",
        "Ada takes metformin.",
        &med("Ada Example", "metformin", "2024-01-01"),
    );
    let v = state(
        dir.path(),
        "Ada Example",
        &["--relation", "takes_medication", "--limit", "1"],
    );
    let groups = v["data"]["state"].as_array().expect("groups");
    assert_eq!(groups.len(), 1, "{v}");
    assert_eq!(
        groups[0]["open_count"],
        json!(2),
        "one value shown, two open — the count must say two: {v}"
    );
    assert_eq!(groups[0]["values"].as_array().map(|a| a.len()), Some(1));
}

#[test]
fn a_known_subject_with_no_data_for_a_relation_is_not_unknown() {
    // The naive-agent wall: `state ada --relation allergic_to` on a store
    // that KNOWS ada (two medications) used to answer `subject_not_found` —
    // while listing ada in nearest_subjects at 1.0. "No allergy data" and
    // "no such person" are different answers.
    let dir = store();
    remember_facts(
        dir.path(),
        "personal",
        "Ada takes metformin.",
        &med("Ada Example", "metformin", "2024-01-01"),
    );
    let v = state(dir.path(), "Ada Example", &["--relation", "allergic_to"]);
    assert_eq!(v["data"]["verdict"], json!("no_match"));
    assert_eq!(v["data"]["resolved_from"], json!("exact"));
    let absence = &v["data"]["absence"];
    assert_eq!(
        absence["reason"],
        json!("no_open_values"),
        "a resolved subject is never subject_not_found: {v}"
    );
    assert!(
        absence["top_score"].is_null() && absence["threshold"].is_null(),
        "no cutoff rejected anything — scores here would be fabricated: {v}"
    );
    assert!(
        absence["nearest_subjects"]
            .as_array()
            .is_none_or(|n| n.is_empty()),
        "'did you mean ada' under an answer about ada argues with itself: {v}"
    );
}

#[test]
fn an_unknown_subject_is_a_certificate_not_an_error() {
    let dir = store();
    remember_facts(
        dir.path(),
        "personal",
        "Adam takes metformin.",
        &med("Adam Example", "metformin", "2024-01-01"),
    );
    let out = engram(dir.path())
        .args(["state", "Zebediah Nobody", "--json"])
        .output()
        .expect("ran");
    assert_eq!(out.status.code(), Some(0), "an absence is an answer");
    let v: Value = serde_json::from_slice(&out.stdout).expect("json");
    assert_eq!(v["data"]["verdict"], json!("no_match"));
    let absence = &v["data"]["absence"];
    assert_eq!(absence["reason"], json!("subject_not_found"));
    assert_eq!(
        absence["searched_chunks"],
        json!(0),
        "state scans no chunks"
    );
    assert!(
        absence["searched_kbs"]
            .as_array()
            .unwrap()
            .contains(&json!("personal")),
        "{absence}"
    );
    assert!((absence["threshold"].as_f64().unwrap() - 0.92).abs() < 1e-6);
    let nearest = absence["nearest_subjects"].as_array().expect("near misses");
    assert!(
        nearest.iter().any(|n| n["subject"] == "Adam Example"),
        "an absence certificate must name the subjects that DO exist: {absence}"
    );
}

#[test]
fn a_fuzzy_answer_says_it_is_about_another_name() {
    let dir = store();
    remember_facts(
        dir.path(),
        "personal",
        "Ada takes metformin.",
        &med("Ada Example", "metformin", "2024-01-01"),
    );
    let v = state(dir.path(), "Ada Exampl", &[]);
    assert_eq!(v["data"]["verdict"], json!("results"));
    assert_eq!(v["data"]["resolved_from"], json!("fuzzy"));
    assert!(v["data"]["resolution_score"].as_f64().unwrap() >= 0.92);
    assert!(
        !v["data"]["nearest_subjects"].as_array().unwrap().is_empty(),
        "an answer about a different name must say so in the fields a caller reads: {v}"
    );
}

/// The regression for passing the literal `"*"` into an equality filter. The
/// DEFAULT invocation was the only broken one, which is exactly how such a bug
/// survives review.
#[test]
fn the_default_wildcard_scope_resolves_and_still_names_near_misses() {
    let dir = store();
    remember_facts(
        dir.path(),
        "work",
        "Ada takes metformin.",
        &med("Ada Example", "metformin", "2024-01-01"),
    );
    let v = state(dir.path(), "Ada Exampl", &[]);
    assert_eq!(v["data"]["resolved_from"], json!("fuzzy"), "{v}");
    assert_eq!(group_values(&v, "takes_medication").len(), 1);

    let miss = state(dir.path(), "Nobody At All", &[]);
    assert_eq!(miss["data"]["verdict"], json!("no_match"));
    assert!(
        !miss["data"]["absence"]["nearest_subjects"]
            .as_array()
            .unwrap()
            .is_empty(),
        "the wildcard scope must still find near misses: {miss}"
    );
}

#[test]
fn as_of_reads_the_past_and_agrees_with_the_present_at_now() {
    let dir = store();
    remember_facts(
        dir.path(),
        "personal",
        "Ada started metformin in January 2024.",
        &med("Ada Example", "metformin", "2024-01-01"),
    );
    remember_facts(
        dir.path(),
        "personal",
        "Ada stopped metformin in June 2025.",
        r#"[{"subject":"Ada Example","predicate":"stopped_taking","object":"metformin",
             "valid_until":"2025-06-01"}]"#,
    );

    let then = state(dir.path(), "Ada Example", &["--as-of", "2024-06-01"]);
    assert_eq!(group_values(&then, "takes_medication").len(), 1, "{then}");
    let after = state(dir.path(), "Ada Example", &["--as-of", "2026-01-01"]);
    assert_eq!(group_values(&after, "takes_medication").len(), 0, "{after}");
    let now = state(dir.path(), "Ada Example", &[]);
    assert_eq!(group_values(&now, "takes_medication").len(), 0);

    // Risk 11: the two code paths must agree at `now`, or `state` has two
    // definitions of "current" — the defect this whole command exists to end.
    let as_of_now = state(
        dir.path(),
        "Ada Example",
        &["--as-of", &chrono::Utc::now().to_rfc3339()],
    );
    assert_eq!(as_of_now["data"]["state"], now["data"]["state"]);
}

/// M14. A naive half-open test makes every `Instant` relation vanish from
/// every `--as-of` query, which the medication test above would never catch.
#[test]
fn a_point_interval_survives_as_of() {
    let dir = store();
    remember_facts(
        dir.path(),
        "personal",
        "Ada was born on 2 April 1988.",
        r#"[{"subject":"Ada Example","predicate":"birthday","object":"1988-04-02",
             "valid_from":"1988-04-02"}]"#,
    );
    for when in ["1988-04-02", "2026-01-01"] {
        let v = state(dir.path(), "Ada Example", &["--as-of", when]);
        assert_eq!(
            group_values(&v, "birthday").len(),
            1,
            "a birthday is true from its instant onward, at {when}: {v}"
        );
    }
}

#[test]
fn include_closed_puts_history_under_the_value_never_beside_it() {
    let dir = store();
    remember_facts(
        dir.path(),
        "personal",
        "Ada lives in Lisbon.",
        r#"[{"subject":"Ada Example","predicate":"lives_in","object":"Lisbon",
             "valid_from":"2020-01-01"}]"#,
    );
    remember_facts(
        dir.path(),
        "personal",
        "Ada moved to Porto in 2024.",
        r#"[{"subject":"Ada Example","predicate":"lives_in","object":"Porto",
             "valid_from":"2024-01-01"}]"#,
    );

    let v = state(dir.path(), "Ada Example", &["--include-closed"]);
    let values = group_values(&v, "lives_in");
    assert_eq!(values.len(), 1, "one current home: {v}");
    assert_eq!(values[0]["object"], json!("Porto"));
    let history = values[0]["history"].as_array().expect("history");
    assert_eq!(history.len(), 1, "{v}");
    assert_eq!(history[0]["object"], json!("Lisbon"));
    assert!(history[0]["valid_until"].is_string(), "{history:?}");
    assert!(history[0]["close_reason"].is_string(), "{history:?}");
    // History is never counted as current.
    assert_eq!(v["data"]["total"], json!(1));
}

#[test]
fn scope_is_honest_about_what_it_searched() {
    let dir = store();
    remember_facts(
        dir.path(),
        "work",
        "Ada takes metformin.",
        &med("Ada Example", "metformin", "2024-01-01"),
    );
    let narrowed = engram(dir.path())
        .args(["state", "Ada Example", "--kb", "personal", "--json"])
        .output()
        .expect("ran");
    assert_eq!(narrowed.status.code(), Some(0));
    let v: Value = serde_json::from_slice(&narrowed.stdout).unwrap();
    assert_eq!(v["data"]["verdict"], json!("no_match"), "{v}");
    assert_eq!(
        v["metadata"]["scope"]["kbs"],
        json!(["personal"]),
        "a KB-scoped miss is a scope result, not proof nothing exists: {v}"
    );
    assert!(
        v["metadata"]["scope"].get("chunks_considered").is_none(),
        "state scores no chunks, so it must not claim a count: {v}"
    );

    let wide = state(dir.path(), "Ada Example", &[]);
    assert_eq!(group_values(&wide, "takes_medication").len(), 1);
    assert!(wide["metadata"]["scope"]["kbs"]
        .as_array()
        .unwrap()
        .contains(&json!("work")));
}

#[test]
fn relation_narrowing_accepts_an_alias_of_the_relation() {
    let dir = store();
    remember_facts(
        dir.path(),
        "personal",
        "Ada is allergic to penicillin.",
        r#"[{"subject":"Ada Example","predicate":"allergic_to","object":"penicillin"}]"#,
    );
    remember_facts(
        dir.path(),
        "personal",
        "Ada takes metformin.",
        &med("Ada Example", "metformin", "2024-01-01"),
    );
    // `has_allergy` canonicalizes to `allergic_to` inside the registry, so
    // a caller cannot get the grouping wrong by typing the long form.
    let v = state(dir.path(), "Ada Example", &["--relation", "has_allergy"]);
    let groups = v["data"]["state"].as_array().unwrap();
    assert_eq!(groups.len(), 1, "{v}");
    assert_eq!(groups[0]["relation"], json!("allergic_to"));
}

/// An extractor that spells a safety relation its own way. `is_allergic_to`
/// and `takes` used to stay relations of their own: no safety flag, no safety
/// gate, and `state --relation allergic_to` answered "no allergy" about a
/// person whose allergy was stored.
#[test]
fn an_extracted_safety_fact_under_another_spelling_is_still_a_safety_fact() {
    let dir = store();
    let out = engram(dir.path())
        .env(
            "ENGRAM_FACT_LLM_STUB",
            r#"{"*":[
                {"subject":"Rosalie Brandtke-Wendel","predicate":"is_allergic_to","object":"penicillin"},
                {"subject":"Rosalie Brandtke-Wendel","predicate":"on_medication","object":"ramipril"},
                {"subject":"Rosalie Brandtke-Wendel","predicate":"has_condition","object":"hypertension"}]}"#,
        )
        .args([
            "remember",
            "Rosalie is allergic to penicillin, has hypertension and takes ramipril.",
            "--kb",
            "personal",
            "--offline",
            "--json",
        ])
        .output()
        .expect("engram ran");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let v = state(dir.path(), "Rosalie Brandtke-Wendel", &[]);
    for (relation, object) in [
        ("allergic_to", "penicillin"),
        ("takes_medication", "ramipril"),
        ("diagnosed_with", "hypertension"),
    ] {
        let group = v["data"]["state"]
            .as_array()
            .unwrap()
            .iter()
            .find(|g| g["relation"] == relation)
            .cloned()
            .unwrap_or_else(|| panic!("no {relation} group: {v}"));
        assert_eq!(group["safety"], json!(true), "{group}");
        assert_eq!(group["values"][0]["object"], json!(object), "{group}");
    }

    let allergies = state(
        dir.path(),
        "Rosalie Brandtke-Wendel",
        &["--relation", "allergic_to"],
    );
    assert_eq!(
        allergies["data"]["verdict"],
        json!("results"),
        "{allergies}"
    );
    assert_eq!(
        group_values(&allergies, "allergic_to")[0]["object"],
        json!("penicillin")
    );
}

/// The safety gate on the caller path. A dose change on a medication is a
/// question for a person, never a silent supersession; under the `takes`
/// spelling it used to be an ordinary relation and skipped that gate.
#[test]
fn a_dose_change_under_another_spelling_is_still_a_safety_question() {
    let dir = store();
    remember_facts(
        dir.path(),
        "personal",
        "Elias takes metformin 500 mg daily.",
        r#"[{"subject":"Elias","predicate":"on_medication","object":"metformin 500 mg daily"}]"#,
    );
    remember_facts(
        dir.path(),
        "personal",
        "Elias's metformin went up to 1000 mg daily.",
        r#"[{"subject":"Elias","predicate":"on_medication","object":"metformin 1000 mg daily"}]"#,
    );
    let v = state(dir.path(), "Elias", &["--relation", "takes_medication"]);
    let questions = v["data"]["open_questions"].as_array().cloned().unwrap();
    assert_eq!(questions.len(), 1, "one member, two doses: {v}");
    assert_eq!(questions[0]["relation"], json!("takes_medication"));
    assert_eq!(questions[0]["safety"], json!(true), "{v}");
    assert_eq!(
        questions[0]["suggested_action"],
        json!("requires_context"),
        "a safety pair is never advised toward a supersession: {v}"
    );
}

/// Import keeps the file's predicate on disk, and refuses a safety close only
/// when it recognises the relation. An allergy spelled `is_allergic_to` used
/// to import CLOSED, and `--relation allergic_to` filtered on the stored
/// spelling, so the answer was "no allergy" twice over.
#[test]
fn an_imported_allergy_under_another_spelling_lands_open_and_is_found() {
    let source = store();
    remember_facts(
        source.path(),
        "personal",
        "Lira is allergic to penicillin.",
        r#"[{"subject":"Lira","predicate":"allergic_to","object":"penicillin"}]"#,
    );
    let mut doc = run_json(source.path(), &["export"])["data"].clone();
    // The file another tool wrote: the allergy under its own spelling, row
    // and journal alike, and a journal close for it.
    let fact_id = doc["facts"][0]["id"].as_str().unwrap().to_string();
    let valid_from = doc["facts"][0]["valid_from"].clone();
    doc["facts"][0]["predicate"] = json!("is_allergic_to");
    doc["facts"][0]["valid_until"] = valid_from.clone();
    doc["facts"][0]["close_reason"] = json!("stopped");
    let mut events = doc["events"].as_array().cloned().unwrap();
    for e in &mut events {
        if e["kind"] == "fact.observed" {
            e["payload"]["relation"] = json!("is_allergic_to");
        }
    }
    events.push(json!({
        "seq": events.len() + 1,
        "id": "11111111-2222-4333-8444-555555555555",
        "ts": "2026-08-02T04:00:00Z",
        "actor": "legacy-tool",
        "kind": "fact.interval_closed",
        "subject_id": fact_id,
        "kb": "personal",
        "diary": "default",
        "payload": {
            "fact_id": fact_id,
            "valid_until": valid_from,
            "reason": "stopped",
            "authority": "person",
            "sets_superseded_by": false,
        }
    }));
    doc["counts"]["events"] = json!(events.len());
    doc["events"] = json!(events);
    let file = source.path().join("legacy.json");
    std::fs::write(&file, serde_json::to_vec(&doc).unwrap()).unwrap();

    let target = store();
    run_json(target.path(), &["import", file.to_str().unwrap()]);
    let v = state(target.path(), "Lira", &["--relation", "allergic_to"]);
    assert_eq!(v["data"]["verdict"], json!("results"), "{v}");
    let values = group_values(&v, "allergic_to");
    assert_eq!(values.len(), 1, "{v}");
    assert_eq!(values[0]["object"], json!("penicillin"));
    assert!(
        values[0]["valid_until"].is_null(),
        "she is still allergic: {v}"
    );
}

/// Import keeps the file's spelling on disk, so a medication can sit under
/// `is_on_medication`. The write path loaded a subject's siblings by the
/// stored predicate, so a later cessation (canonical `takes_medication`)
/// never saw that row: it recorded a separate closed interval and the
/// stopped medication stayed current.
#[test]
fn a_cessation_closes_a_medication_stored_under_another_spelling() {
    let source = store();
    remember_facts(
        source.path(),
        "personal",
        "Elias started metformin in January 2024.",
        &med("Elias", "metformin", "2024-01-01"),
    );
    let mut doc = run_json(source.path(), &["export"])["data"].clone();
    doc["facts"][0]["predicate"] = json!("is_on_medication");
    let mut events = doc["events"].as_array().cloned().unwrap();
    for e in &mut events {
        if e["kind"] == "fact.observed" {
            e["payload"]["relation"] = json!("is_on_medication");
        }
    }
    doc["events"] = json!(events);
    let file = source.path().join("legacy.json");
    std::fs::write(&file, serde_json::to_vec(&doc).unwrap()).unwrap();

    let target = store();
    run_json(target.path(), &["import", file.to_str().unwrap()]);
    assert_eq!(
        group_values(&state(target.path(), "Elias", &[]), "takes_medication").len(),
        1,
        "the imported medication is current before the cessation"
    );
    remember_facts(
        target.path(),
        "personal",
        "Elias stopped metformin in June 2025.",
        r#"[{"subject":"Elias","predicate":"stopped_taking","object":"metformin",
             "valid_until":"2025-06-01"}]"#,
    );
    let v = state(target.path(), "Elias", &[]);
    assert_eq!(
        group_values(&v, "takes_medication").len(),
        0,
        "a stopped medication is not current: {v}"
    );
    let all = state(target.path(), "Elias", &["--include-closed"]);
    let group = all["data"]["state"]
        .as_array()
        .unwrap()
        .iter()
        .find(|g| g["relation"] == "takes_medication")
        .cloned()
        .unwrap_or_else(|| panic!("no takes_medication group: {all}"));
    let history = group["history"].as_array().unwrap();
    assert_eq!(
        history.len(),
        1,
        "one interval, closed — no second row: {all}"
    );
    assert_eq!(history[0]["id"], doc["facts"][0]["id"], "{all}");
    assert_eq!(
        history[0]["valid_until"],
        json!("2025-06-01T00:00:00+00:00")
    );
}

/// The isolation gate (CD-6), asserted rather than trusted.
#[test]
fn this_suite_never_reaches_for_the_real_store() {
    // The needle is assembled at runtime so this assertion cannot match
    // itself — a grep gate that trips on its own source is a gate nobody
    // keeps.
    let banned = format!("point{}at{}home", "_", "_");
    for src in [
        include_str!("state_view.rs"),
        include_str!("state_support/mod.rs"),
    ] {
        assert!(!src.contains(&banned), "no HOME rewriting in this suite");
    }
    let support = include_str!("state_support/mod.rs");
    assert!(support.contains("ENGRAM_STORE"), "isolation is the env var");
    assert!(
        support.contains("env_remove(\"ENGRAM_HOME\")") || support.contains("\"ENGRAM_HOME\","),
        "the dead name is removed, never set"
    );
}

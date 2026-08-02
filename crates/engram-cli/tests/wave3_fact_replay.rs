//! Wave 3 — a question is asked once.
//!
//! v0.3.1 keyed a conflict by the two FACT ROWS, so a re-derivation with fresh
//! UUIDs — after `edit --content`, `facts backfill`, or a second memory saying
//! the same thing — asked the same question again. Nothing recorded WHO
//! decided, and the FK cascade that erased the queue erased the only trace.
//!
//! The key is now SEMANTIC: `(subject_norm, relation, object_pair_hash)`. Two
//! consequences are tested here — the same question is never queued twice, and
//! the key survives a re-derivation with different rows.

mod wave3_fact_support;
use wave3_fact_support::*;

const CAROL: &str = r#"[{"subject":"Carol","predicate":"works_at","object":"Acme"},
                        {"subject":"Carol","predicate":"works_at","object":"Beta Corp"}]"#;

#[test]
fn the_same_question_is_never_queued_twice() {
    let tmp = store();
    let first = remember_facts(
        tmp.path(),
        "One memo says Carol is at Acme, the next line says Beta Corp.",
        CAROL,
    );
    let queued = first["data"]["adjudication_needed"].as_array().unwrap();
    assert_eq!(queued.len(), 1, "two employers from one memo: {first}");
    let original_id = queued[0]["conflict_id"].as_str().unwrap().to_string();
    assert!(
        queued[0]["deduplicated"].is_null(),
        "the first asking is not a duplicate: {}",
        queued[0]
    );

    // A SECOND memory restates the same contradiction. New rows, new uuids,
    // the same question — and the queue must not grow.
    let second = remember_facts(
        tmp.path(),
        "A later note repeats it: Carol at Acme, then Beta Corp.",
        CAROL,
    );
    let queued = second["data"]["adjudication_needed"].as_array().unwrap();
    assert_eq!(
        open_conflicts(tmp.path())["data"]["total_open"],
        1,
        "the storm is a question asked once per re-derivation: {second}"
    );
    // Whatever it reports, it must name the question that already exists —
    // never an exit-0 success with the question silently missing.
    for entry in queued {
        assert_eq!(
            entry["conflict_id"].as_str().unwrap(),
            original_id,
            "a re-asked question must name the row already open: {entry}"
        );
        assert_eq!(
            entry["deduplicated"], true,
            "storage matched the semantic KEY, which is the only honest way to \
             claim this: {entry}"
        );
    }
}

/// The key is over the canonical relation and the two normalized objects, so a
/// synonym predicate and different casing key the same question.
#[test]
fn the_key_survives_a_re_derivation_that_spells_things_differently() {
    let tmp = store();
    let first = remember_facts(
        tmp.path(),
        "One memo says Carol is at Acme, the next line says Beta Corp.",
        CAROL,
    );
    let original_id = first["data"]["adjudication_needed"][0]["conflict_id"]
        .as_str()
        .unwrap()
        .to_string();

    let second = remember_facts(
        tmp.path(),
        "Re-read of the same memo: Carol employed by acme, then beta corp.",
        r#"[{"subject":"Carol","predicate":"employed_by","object":"acme"},
            {"subject":"Carol","predicate":"employed_by","object":"beta corp"}]"#,
    );
    assert_eq!(
        open_conflicts(tmp.path())["data"]["total_open"],
        1,
        "`employed_by` canonicalizes to `works_at`, so this is the same \
         question: {second}"
    );
    for entry in second["data"]["adjudication_needed"].as_array().unwrap() {
        assert_eq!(entry["conflict_id"].as_str().unwrap(), original_id);
    }
}

/// A coexisting pair is not a question, so a second telling of it queues
/// nothing at all — the key never even gets consulted.
#[test]
fn a_repeated_coexistence_never_becomes_a_question() {
    let tmp = store();
    let facts = r#"[{"subject":"Ana","predicate":"allergic_to","object":"penicillin"},
                    {"subject":"Ana","predicate":"allergic_to","object":"amoxicillin"}]"#;
    remember_facts(
        tmp.path(),
        "Ana is allergic to penicillin and amoxicillin.",
        facts,
    );
    remember_facts(tmp.path(), "Her chart repeats both allergies.", facts);
    assert_eq!(open_conflicts(tmp.path())["data"]["total_open"], 0);
    assert_eq!(current_facts(tmp.path(), "Ana").len(), 2);
}

/// Replay across a re-derivation, end to end: answer once, then let the same
/// pair be derived again and watch the store apply the answer instead of
/// asking.
///
/// Unblocked in wave 4. `facts resolve` now runs through
/// `Core::resolve_conflict`, which wraps storage's
/// `resolve_conflict_with_decision` (D8), so a `caller` decision exists in
/// `fact_decisions` for the pair's semantic key and the planner can find it.
///
/// One correction against the blocked draft: `remember` does not emit
/// `plan_notes`, so the replay is asserted where it is actually visible — the
/// per-pair `resolution` on `data.conflicts`, rather than
/// `conflicts_queued: 1`. A silent replay is still refused: the row says which
/// decision was applied.
///
/// WHICH `replayed_*` tag lands is not pinned, and that is G3. The tag names
/// what happened to THIS pair, and a re-derivation decides for itself which of
/// the two values it emits first — so the same ruling reads `close_old` in one
/// orientation and `reject_new` in the other. What must not move is the
/// OUTCOME: the caller retired Acme, so Acme stays retired either way.
#[test]
fn a_resolved_question_is_never_asked_again() {
    let tmp = store();
    let first = remember_facts(
        tmp.path(),
        "One memo says Carol is at Acme, the next line says Beta Corp.",
        CAROL,
    );
    let conflict_id = first["data"]["adjudication_needed"][0]["conflict_id"]
        .as_str()
        .unwrap()
        .to_string();
    run_json(
        &mut engram(tmp.path()),
        &[
            "facts",
            "resolve",
            &conflict_id,
            "--action",
            "supersede-old",
        ],
    );

    // Re-derive the same pair from a different memory. The caller answered
    // this; nobody should be asked twice.
    let again = remember_facts(
        tmp.path(),
        "Another note: Carol at Acme, then Beta Corp.",
        CAROL,
    );
    assert_eq!(
        open_conflicts(tmp.path())["data"]["total_open"],
        0,
        "a question the caller answered must not reopen: {again}"
    );
    assert_eq!(
        again["metadata"]["conflicts_queued"], 0,
        "the answer is replayed, not re-asked: {again}"
    );
    let applied = again["data"]["conflicts"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        !applied.is_empty()
            && applied.iter().all(|c| c["resolution"]
                .as_str()
                .is_some_and(|r| r.starts_with("replayed_"))),
        "a silent replay is indistinguishable from a dropped question — each \
         pair must name the decision it applied: {again}"
    );
    let employers: Vec<String> = current_facts(tmp.path(), "Carol")
        .iter()
        .filter_map(|f| f["object"].as_str().map(str::to_string))
        .collect();
    assert_eq!(
        employers,
        vec!["Beta Corp"],
        "the caller retired Acme; a replay applies that answer, never its \
         mirror image: {again}"
    );
}

/// The other arm of the same claim: `keep-both` is a decision too. It used to
/// write nothing at all, so the caller's answer lived only in a queue row's
/// status and every re-derivation asked again.
#[test]
fn keep_both_is_an_answer_the_store_remembers() {
    let tmp = store();
    let first = remember_facts(
        tmp.path(),
        "One memo says Carol is at Acme, the next line says Beta Corp.",
        CAROL,
    );
    let conflict_id = first["data"]["adjudication_needed"][0]["conflict_id"]
        .as_str()
        .unwrap()
        .to_string();
    let resolved = run_json(
        &mut engram(tmp.path()),
        &["facts", "resolve", &conflict_id, "--action", "keep-both"],
    );
    assert!(
        resolved["data"]["decision_id"].is_string(),
        "keep-both must leave a decision behind: {resolved}"
    );
    remember_facts(
        tmp.path(),
        "Another note: Carol at Acme, then Beta Corp.",
        CAROL,
    );
    assert_eq!(
        open_conflicts(tmp.path())["data"]["total_open"],
        0,
        "both values stay live and the question stays answered"
    );
}

/// The re-gate's blocker B, as the CLI reaches it. A `kb delete` cascades the
/// conflict row away through the two fact foreign keys, while the decision —
/// which has no foreign key — survives under a name the caller can reuse. So
/// the ruling outlives the only row that said which side of it lost.
///
/// Before G3's storage half, replay then fell back to pair ORDER: the caller
/// had kept `metformin 1000 mg`, the re-derivation named the doses the other
/// way round, and "close the old one" closed the 1000 mg. A reversed
/// prescription, applied silently, on the relation where that matters most.
///
/// The decision now carries the value it retired, so the outcome travels with
/// the dose instead of with the slot.
#[test]
fn a_purged_conflict_row_cannot_reverse_a_prescription() {
    const ASCENDING: &str = r#"[{"subject":"Ana","predicate":"takes_medication","object":"metformin 500 mg"},
                                {"subject":"Ana","predicate":"takes_medication","object":"metformin 1000 mg"}]"#;
    const DESCENDING: &str = r#"[{"subject":"Ana","predicate":"takes_medication","object":"metformin 1000 mg"},
                                 {"subject":"Ana","predicate":"takes_medication","object":"metformin 500 mg"}]"#;
    let tmp = store();
    let remember = |facts: &str, content: &str| {
        run_json(
            &mut engram(tmp.path()),
            &[
                "remember",
                content,
                "--kb",
                "medical",
                "--offline",
                "--facts-json",
                facts,
            ],
        )
    };
    let doses = || -> Vec<String> {
        let mut v: Vec<String> = run_json(
            &mut engram(tmp.path()),
            &["facts", "list", "--subject", "Ana", "--kb", "medical"],
        )["data"]["facts"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|f| f["object"].as_str().map(str::to_string))
            .collect();
        v.sort();
        v
    };

    run_json(&mut engram(tmp.path()), &["kb", "create", "medical"]);
    let first = remember(
        ASCENDING,
        "Ana takes metformin 500 mg and metformin 1000 mg.",
    );
    let conflict_id = first["data"]["adjudication_needed"][0]["conflict_id"]
        .as_str()
        .expect("a dose change on a safety relation is a question")
        .to_string();
    run_json(
        &mut engram(tmp.path()),
        &[
            "facts",
            "resolve",
            &conflict_id,
            "--action",
            "supersede-old",
        ],
    );
    assert_eq!(doses(), vec!["metformin 1000 mg".to_string()]);

    // The purge. Recreating the KB under the same name is what lets the
    // orphaned decision meet a fresh derivation of its own question.
    run_json(
        &mut engram(tmp.path()),
        &["kb", "delete", "medical", "--confirm"],
    );
    run_json(&mut engram(tmp.path()), &["kb", "create", "medical"]);
    let reversed = remember(
        DESCENDING,
        "Reversed after purge: metformin 1000 mg and metformin 500 mg.",
    );
    assert_eq!(
        doses(),
        vec!["metformin 1000 mg".to_string()],
        "the caller kept the 1000 mg dose; no re-derivation order may retire \
         it: {reversed}"
    );
}

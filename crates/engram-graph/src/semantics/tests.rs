//! The classifier, the keys, and the planner — all provable from fixtures.
//!
//! Nothing here opens a store. That is a design property, not an accident:
//! the polypharmacy guarantee has to be checkable without SQLite or nobody
//! will check it.

use super::*;
use chrono::TimeZone;
use std::collections::HashMap;

fn ts(s: &str) -> DateTime<Utc> {
    if s.len() == 10 {
        let d: Vec<i32> = s.split('-').map(|p| p.parse().unwrap()).collect();
        return Utc
            .with_ymd_and_hms(d[0], d[1] as u32, d[2] as u32, 0, 0, 0)
            .unwrap();
    }
    DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
}

/// A stored fact. Every fact gets its own source memory unless a test pins
/// one, because "came out of the same text" is itself a signal.
fn stored(subject: &str, predicate: &str, object: &str, at: &str) -> Fact {
    let t = ts(at);
    Fact {
        id: Uuid::new_v4(),
        source_memory_id: Uuid::new_v4(),
        subject: subject.into(),
        subject_norm: subject.trim().to_lowercase(),
        predicate: predicate.into(),
        object: object.into(),
        object_norm: object.trim().to_lowercase(),
        confidence: 1.0,
        created_at: t,
        superseded_by: None,
        superseded_at: None,
        diary: "default".into(),
        fact_type: FactType::Assertion,
        valid_from: Some(t),
        valid_until: None,
        resolution: None,
        source_kind: SourceKind::AgentExtracted,
        confirmations: 0,
        closed_by: None,
        close_reason: None,
        close_authority: None,
        kb: "personal".into(),
    }
}

fn closed(mut f: Fact, until: &str) -> Fact {
    f.valid_until = Some(ts(until));
    f.close_reason = Some("stopped".into());
    f.close_authority = Some("explicit_end".into());
    f
}

fn triage(new: &Fact, old: &Fact) -> Verdict {
    classify_pair(new, old, None, ClassifyMode::Triage)
}

fn write(new: &Fact, old: &Fact) -> Verdict {
    classify_pair(new, old, None, ClassifyMode::Write)
}

fn observation_of(f: &Fact) -> Observation {
    let relation = canonical_relation(&f.predicate);
    Observation {
        fact_id: f.id,
        subject: f.subject.clone(),
        subject_norm: f.subject_norm.clone(),
        spec: relation_spec(&relation),
        member: member_identity(&relation, &f.object_norm),
        relation,
        object: f.object.clone(),
        object_norm: f.object_norm.clone(),
        confidence: f.confidence,
        source_memory_id: f.source_memory_id,
        source_kind: f.source_kind,
        fact_type: f.fact_type,
        diary: f.diary.clone(),
        kb: f.kb.clone(),
        interval: Interval::of(f),
        state: ObservationState::Ongoing,
        autonomy: Autonomy::Assisted,
        queue_everything: false,
    }
}

/// A cessation THE PERSON stated — a `--facts-json` triple, or a value they
/// confirmed. Only this provenance carries `CloseAuthority::ExplicitEnd` [G1],
/// so every fixture that means "the text said it ended" has to say so.
fn stated_end(f: &Fact, until: &str) -> Observation {
    let mut o = observation_of(f);
    o.fact_id = Uuid::new_v4();
    o.source_kind = SourceKind::UserStated;
    o.state = ObservationState::Ended;
    o.interval.until = Some(ts(until));
    o
}

/// The same utterance, arriving from the EXTRACTION MODEL instead.
fn extracted_end(f: &Fact, until: &str) -> Observation {
    Observation {
        source_kind: SourceKind::AgentExtracted,
        ..stated_end(f, until)
    }
}

/// No prior decisions at all — the common case, and the one that must not
/// need a store.
fn no_decisions(_: &SemanticKey) -> Result<Option<DecisionRecord>, StorageError> {
    Ok(None)
}

fn plan(observation: &Observation, siblings: &[Fact]) -> WritePlan {
    plan_write(
        observation,
        siblings,
        &[],
        ClassifyMode::Write,
        &no_decisions,
    )
    .expect("a fixture plan never fails")
}

// ===========================================================================
// C. Classification
// ===========================================================================

/// THE DEFECT, reproduced and killed. Five medications, ten pairs, zero
/// conflicts — and the fifth write compares against nothing.
#[test]
fn five_medications_produce_no_conflicts() {
    let meds = [
        "empagliflozin 12.5 mg daily",
        "rapamycin 6 mg weekly",
        "metformin 500 mg twice daily",
        "lisinopril 10 mg daily",
        "levothyroxine 75 mcg daily",
    ];
    let facts: Vec<Fact> = meds
        .iter()
        .enumerate()
        .map(|(i, m)| stored("Ana", "takes_medication", m, &format!("2026-0{}-01", i + 1)))
        .collect();
    for (i, a) in facts.iter().enumerate() {
        for (j, b) in facts.iter().enumerate() {
            if i == j {
                continue;
            }
            let v = triage(a, b);
            assert_eq!(
                v.class,
                ConflictClass::Coexist,
                "{} vs {} classified {:?}/{}",
                a.object,
                b.object,
                v.class,
                v.rule
            );
            assert_eq!(v.rule, "safety_set_coexist");
        }
    }
    // …and the fifth write plans nothing at all.
    let fifth = observation_of(&facts[4]);
    let p = plan(&fifth, &facts[..4]);
    assert!(p.queue.is_empty(), "{:?}", p.queue);
    assert!(p.closes.is_empty(), "{:?}", p.closes);
    assert!(p.insert.is_some(), "the fifth medication is still stored");
}

#[test]
fn same_drug_new_dose_is_a_question_not_a_supersession() {
    let old = stored(
        "Ana",
        "takes_medication",
        "empagliflozin 12.5 mg daily",
        "2026-01-01",
    );
    let new = stored(
        "Ana",
        "takes_medication",
        "empagliflozin 25 mg daily",
        "2026-06-01",
    );
    let v = triage(&new, &old);
    assert_eq!(v.class, ConflictClass::Queue);
    assert_eq!(v.rule, "safety_member_change");
    let (nv, ov) = (FactView::of(&new), FactView::of(&old));
    assert_eq!(suggested_action(&v, &nv, &ov), "requires_context");
    let p = plan(&observation_of(&new), &[old]);
    assert!(p.closes.is_empty(), "a dose change closes nothing");
    assert_eq!(p.queue.len(), 1);
    assert_eq!(p.queue[0].suggested, "requires_context");
}

#[test]
fn a_dose_change_on_a_routine_set_relation_closes_the_old_interval() {
    // `works_on` is set-valued and NOT safety, so a same-member change is an
    // ordinary update.
    let old = stored("Ana", "works_on", "engram cli", "2026-01-01");
    let new = stored("Ana", "works_on", "engram cli v2", "2026-06-01");
    // Different members under the v1 heuristic (it only strips from the END),
    // so they coexist — which is the honest answer and the one risk 12 names.
    assert_eq!(triage(&new, &old).class, ConflictClass::Coexist);

    // A true same-member change: only the trailing dose-shaped token differs.
    let old = stored("Ana", "supplements_with", "vitamin d 1000 iu", "2026-01-01");
    let new = stored("Ana", "supplements_with", "vitamin d 2000 iu", "2026-06-01");
    let v = triage(&new, &old);
    assert_eq!(v.class, ConflictClass::SupersedeOld);
    assert_eq!(v.rule, "member_attribute_change");
    let p = plan(&observation_of(&new), std::slice::from_ref(&old));
    assert_eq!(p.closes.len(), 1);
    assert_eq!(p.closes[0].fact_id, old.id);
    assert_eq!(p.closes[0].authority, CloseAuthority::SuccessorInterval);
}

/// Property test: nothing the classifier can emit closes a safety interval.
#[test]
fn no_verdict_ever_closes_a_safety_interval() {
    let relations = [
        "takes_medication",
        "allergic_to",
        "diagnosed_with",
        "blood_type",
    ];
    let objects = [
        ("penicillin", "amoxicillin"),
        ("metformin 500 mg", "metformin 1000 mg"),
        ("o+", "a-"),
        ("type 2 diabetes", "type 2 diabetes stage 2"),
    ];
    let times = [("2020-01-01", "2026-01-01"), ("2026-01-01", "2020-01-01")];
    let kinds = [
        SourceKind::UserStated,
        SourceKind::UserConfirmed,
        SourceKind::AgentExtracted,
        SourceKind::CompileLlm,
        SourceKind::Import,
    ];
    for relation in relations {
        for (a, b) in objects {
            for (ta, tb) in times {
                for kind in kinds {
                    for mode in [ClassifyMode::Write, ClassifyMode::Triage] {
                        let mut new = stored("Ana", relation, a, ta);
                        let old = stored("Ana", relation, b, tb);
                        new.source_kind = kind;
                        new.confidence = 1.0;
                        let v = classify_pair(&new, &old, Some(0.99), mode);
                        assert!(
                            !matches!(
                                v.class,
                                ConflictClass::SupersedeOld | ConflictClass::SupersedeNew
                            ),
                            "{relation} {a}/{b} {kind:?} {mode:?} → {:?}/{}",
                            v.class,
                            v.rule
                        );
                    }
                }
            }
        }
    }
}

/// R7's prohibition, at full strength and scoped where R7 actually scopes it.
#[test]
fn suggested_action_is_never_supersede_old_on_a_safety_pair() {
    let classes = [
        ConflictClass::Coexist,
        ConflictClass::Duplicate,
        ConflictClass::SupersedeOld,
        ConflictClass::SupersedeNew,
        ConflictClass::Settled,
        ConflictClass::Queue,
    ];
    let rules = [
        "safety_slot",
        "safety_member_change",
        "safety_set_coexist",
        "slot_lww",
        "recency_supersede",
        "comparable_values",
        "duplicate_object",
    ];
    let new = stored("Ana", "takes_medication", "metformin 500 mg", "2026-01-01");
    let old = stored("Ana", "takes_medication", "metformin 1000 mg", "2020-01-01");
    let (nv, ov) = (FactView::of(&new), FactView::of(&old));
    for class in classes {
        for rule in rules {
            for from in [
                None,
                Some(ConflictClass::SupersedeOld),
                Some(ConflictClass::Duplicate),
            ] {
                let v = Verdict {
                    class,
                    rule,
                    escalated_from: from,
                };
                assert_eq!(
                    suggested_action(&v, &nv, &ov),
                    "requires_context",
                    "{class:?}/{rule}/{from:?} advised a supersession on a safety pair"
                );
            }
        }
    }
}

/// The positive half, and the only test that proves R7's honest case fires.
/// Without it the blanket rule can be reintroduced silently and every other
/// test in this file stays green.
#[test]
fn a_strict_escalated_slot_update_still_suggests_supersede_old() {
    let old = stored("Elias", "lives_in", "London", "2015-06-01");
    let new = stored("Elias", "lives_in", "Lisbon", "2024-02-01");
    let mut observation = observation_of(&new);
    observation.autonomy = Autonomy::Strict;
    let p = plan(&observation, &[old]);
    assert!(p.closes.is_empty(), "strict never auto-supersedes");
    assert_eq!(p.queue.len(), 1);
    assert_eq!(
        p.queue[0].verdict.escalated_from,
        Some(ConflictClass::SupersedeOld),
        "the class the classifier actually reached must survive the escalation"
    );
    assert_eq!(
        p.queue[0].suggested, "supersede-old",
        "the classifier ordered these values; hiding that grows the queue for nothing"
    );
}

#[test]
fn disjoint_intervals_coexist() {
    let london = closed(
        stored("Elias", "lives_in", "London", "2015-06-01"),
        "2020-01-01",
    );
    let lisbon = stored("Elias", "lives_in", "Lisbon", "2024-02-01");
    let v = triage(&lisbon, &london);
    assert_eq!(v.class, ConflictClass::Coexist);
    assert_eq!(v.rule, "disjoint_intervals");
    let p = plan(&observation_of(&lisbon), &[london]);
    assert!(p.closes.is_empty(), "history is not a contradiction");
    assert!(p.queue.is_empty());
}

#[test]
fn overlapping_single_valued_intervals_close_the_older_one() {
    let london = stored("Elias", "lives_in", "London", "2015-06-01");
    let lisbon = stored("Elias", "lives_in", "Lisbon", "2024-02-01");
    let v = triage(&lisbon, &london);
    assert_eq!(v.class, ConflictClass::SupersedeOld);
    assert_eq!(v.rule, "recency_supersede");
    let p = plan(&observation_of(&lisbon), std::slice::from_ref(&london));
    assert_eq!(p.closes.len(), 1);
    assert_eq!(p.closes[0].fact_id, london.id);
    assert_eq!(
        p.closes[0].at,
        ts("2024-02-01"),
        "the close instant is the successor's valid_from"
    );
    assert_eq!(p.closes[0].closed_by, Some(lisbon.id));
    // Backfilling an OLD memory must not overwrite the current value.
    let v = triage(&london, &lisbon);
    assert_eq!(v.class, ConflictClass::SupersedeNew);
    assert_eq!(v.rule, "recency_stale");
}

/// The 951-drain rule. In v0.3.1 this returned `Queue("safety_slot")` forever:
/// the safety early-return fired before the settled check, so a pair with
/// nothing left to decide could never leave the queue.
#[test]
fn an_already_settled_safety_pair_is_settled_not_queued() {
    let mut old = stored("Ana", "allergic_to", "penicillin", "2020-01-01");
    let new = stored("Ana", "allergic_to", "amoxicillin", "2026-01-01");
    old.superseded_by = Some(Uuid::new_v4());
    let v = triage(&new, &old);
    assert_eq!(v.class, ConflictClass::Settled);
    assert_eq!(v.rule, "already_settled");
    assert!(v.is_auto_resolvable());
}

#[test]
fn two_birthdays_are_a_contradiction_no_clock_can_settle() {
    let old = stored("Ana", "birthday", "1990-04-12", "2020-01-01");
    let new = stored("Ana", "birthday", "1991-04-12", "2026-01-01");
    for mode in [ClassifyMode::Write, ClassifyMode::Triage] {
        let v = classify_pair(&new, &old, None, mode);
        assert_eq!(v.class, ConflictClass::Queue, "{mode:?}");
        assert_eq!(v.rule, "immutable_attribute_disagreement");
    }
    let (nv, ov) = (FactView::of(&new), FactView::of(&old));
    assert_eq!(
        suggested_action(&triage(&new, &old), &nv, &ov),
        "requires_context"
    );
}

#[test]
fn a_user_correction_supersedes_an_extracted_birthday() {
    let old = stored("Ana", "birthday", "1990-04-12", "2020-01-01");
    let mut new = stored("Ana", "birthday", "1991-04-12", "2026-01-01");
    new.source_kind = SourceKind::UserStated;
    let v = write(&new, &old);
    assert_eq!(v.class, ConflictClass::SupersedeOld);
    assert_eq!(v.rule, "correction_by_user");
    // A person correcting an immutable value is CALLER evidence, so the
    // interval gate lets it through where a successor would be refused.
    let p = plan(&observation_of(&new), std::slice::from_ref(&old));
    assert_eq!(p.closes.len(), 1);
    assert_eq!(p.closes[0].reason, CloseReason::Corrected);
    assert!(matches!(
        p.closes[0].authority,
        CloseAuthority::Caller { .. }
    ));
    // …but a blood type is safety, so the same correction queues.
    let old_bt = stored("Ana", "blood_type", "o+", "2020-01-01");
    let mut new_bt = stored("Ana", "blood_type", "a-", "2026-01-01");
    new_bt.source_kind = SourceKind::UserStated;
    let v = write(&new_bt, &old_bt);
    assert_eq!(v.class, ConflictClass::Queue);
    assert_eq!(v.rule, "safety_slot");
    assert_eq!(v.escalated_from, Some(ConflictClass::SupersedeOld));
}

#[test]
fn a_reinstated_medication_is_a_second_interval_not_a_duplicate() {
    let stopped = closed(
        stored("Ana", "takes_medication", "metformin", "2024-01-01"),
        "2025-06-01",
    );
    let restarted = stored("Ana", "takes_medication", "metformin", "2026-01-01");
    let v = triage(&restarted, &stopped);
    assert_eq!(v.class, ConflictClass::Coexist);
    assert_eq!(v.rule, "reinstated_interval");
    let p = plan(&observation_of(&restarted), &[stopped]);
    assert!(p.insert.is_some(), "the restart opens a second interval");
    assert!(p.queue.is_empty());
    assert!(p.closes.is_empty());
}

/// An absorbed observation takes its whole plan with it.
///
/// "Ana lives in Paris" closes the open London interval on its way past — and
/// is then absorbed by a Paris interval somebody deliberately ended in 2020.
/// Keeping the close would retire the CURRENT value on the strength of an
/// assertion the planner just discarded.
#[test]
fn an_absorbed_observation_closes_nothing_on_its_way_out() {
    let london = stored("Ana", "lives_in", "London", "2015-01-01");
    let paris_then = closed(
        stored("Ana", "lives_in", "Paris", "2018-01-01"),
        "2020-01-01",
    );
    let paris_again = stored("Ana", "lives_in", "Paris", "2019-01-01");
    let p = plan(
        &observation_of(&paris_again),
        &[london.clone(), paris_then.clone()],
    );
    assert_eq!(p.absorbed, Some("restates_closed_interval"));
    assert!(p.insert.is_none());
    assert!(
        p.closes.is_empty(),
        "the London interval must survive an assertion nobody kept: {:?}",
        p.closes
    );
    assert!(p.queue.is_empty());
}

#[test]
fn a_backdated_assertion_inside_a_closed_window_is_settled() {
    let stopped = closed(
        stored("Ana", "takes_medication", "metformin", "2024-01-01"),
        "2025-06-01",
    );
    let backdated = stored("Ana", "takes_medication", "metformin", "2024-09-01");
    let v = triage(&backdated, &stopped);
    assert_eq!(v.class, ConflictClass::Settled);
    assert_eq!(v.rule, "restates_closed_interval");
    let p = plan(&observation_of(&backdated), &[stopped]);
    assert!(
        p.insert.is_none(),
        "re-opening a window someone deliberately ended is the failure this arm exists to stop"
    );
    assert_eq!(p.absorbed, Some("restates_closed_interval"));
}

/// The gate covers `Duplicate` because a backlog sweep resolves a duplicate by
/// SUPERSEDING the incoming row. The write planner absorbs it by confirming
/// instead, so a re-asserted allergy is counted, never queued and never closed.
#[test]
fn a_safety_duplicate_is_confirmed_not_superseded() {
    let old = stored("Ana", "allergic_to", "penicillin", "2020-01-01");
    let new = stored("Ana", "allergic_to", "Penicillin", "2026-01-01");
    let v = triage(&new, &old);
    assert_eq!(
        v.class,
        ConflictClass::Queue,
        "the sweep may not supersede a safety row behind a duplicate verdict"
    );
    assert_eq!(v.escalated_from, Some(ConflictClass::Duplicate));

    let p = plan(&observation_of(&new), std::slice::from_ref(&old));
    assert_eq!(p.confirm, vec![old.id]);
    assert!(p.insert.is_none(), "no second identical allergy row");
    assert!(p.closes.is_empty(), "a confirmation closes nothing");
    assert!(
        p.queue.is_empty(),
        "asking the caller to adjudicate penicillin against penicillin is the storm in a new hat"
    );

    // The same is true off a safety relation.
    let old = stored("Elias", "works_at", "Acme", "2026-01-01");
    let new = stored("Elias", "works_at", "acme", "2026-02-01");
    assert_eq!(triage(&new, &old).class, ConflictClass::Duplicate);
    let p = plan(&observation_of(&new), std::slice::from_ref(&old));
    assert!(p.closes.is_empty(), "a duplicate no longer supersedes");
    assert_eq!(p.confirm, vec![old.id]);
}

#[test]
fn strict_autonomy_escalates_heuristic_closes_but_not_explicit_ends() {
    let london = stored("Elias", "lives_in", "London", "2015-06-01");
    let lisbon = stored("Elias", "lives_in", "Lisbon", "2024-02-01");
    let mut strict = observation_of(&lisbon);
    strict.autonomy = Autonomy::Strict;
    let p = plan(&strict, std::slice::from_ref(&london));
    assert!(p.closes.is_empty());
    assert_eq!(p.queue.len(), 1);

    // An explicit end is the TEXT speaking, and strict never promised to
    // disbelieve the text.
    let mut ended = stated_end(&lisbon, "2020-01-01");
    ended.fact_id = lisbon.id;
    ended.autonomy = Autonomy::Strict;
    ended.object_norm = "london".into();
    ended.member = member_identity("lives_in", "london");
    let p = plan(&ended, std::slice::from_ref(&london));
    assert_eq!(p.closes.len(), 1, "{p:?}");
    assert_eq!(p.closes[0].authority, CloseAuthority::ExplicitEnd);
    assert!(p.queue.is_empty());
}

#[test]
fn queue_everything_forces_every_close_into_the_queue() {
    let london = stored("Elias", "lives_in", "London", "2015-06-01");
    let lisbon = stored("Elias", "lives_in", "Lisbon", "2024-02-01");
    let mut seam = observation_of(&lisbon);
    seam.queue_everything = true;
    let p = plan(&seam, &[london]);
    assert!(p.closes.is_empty());
    assert_eq!(p.queue.len(), 1);
    assert_eq!(
        p.queue[0].verdict.escalated_from, None,
        "a test seam must not change the advice a caller is given"
    );
    assert_eq!(p.queue[0].suggested, "keep-both");
}

// ----- the surviving v0.3.1 rules, moved -----

#[test]
fn second_value_of_a_multi_valued_relation_is_not_a_conflict() {
    let old = stored("Elias", "knows", "Alice", "2026-01-01");
    let new = stored("Elias", "knows", "Bob", "2026-02-01");
    let v = triage(&new, &old);
    assert_eq!(v.class, ConflictClass::Coexist);
    assert_eq!(v.rule, "multi_valued");
    assert_eq!(write(&new, &old).class, ConflictClass::Coexist);
    for p in MULTI_VALUED_PREDICATES {
        assert!(
            relation_spec(p).is_set_valued(),
            "{p} must stay set-valued — treating it as a slot turned an address book into a queue"
        );
    }
}

#[test]
fn different_subjects_never_conflict() {
    let old = stored("Alice", "lives_in", "Berlin", "2026-01-01");
    let new = stored("Bob", "lives_in", "Porto", "2026-02-01");
    assert_eq!(triage(&new, &old).rule, "different_subject");
    // …including on a safety relation, where the verdict touches no row.
    let old = stored("Ana", "allergic_to", "penicillin", "2020-01-01");
    let bob = stored("Bob", "allergic_to", "peanuts", "2026-01-01");
    assert_eq!(triage(&bob, &old).rule, "different_subject");
    assert_eq!(triage(&bob, &old).class, ConflictClass::Coexist);
}

#[test]
fn different_relations_coexist_unless_they_are_one_slot_paraphrased() {
    let old = stored("Elias", "lives_in", "Lisbon", "2026-01-01");
    let new = stored("Elias", "has_role", "CEO", "2026-02-01");
    assert_eq!(triage(&new, &old).rule, "different_predicate");
    let new = stored("Elias", "home_city_is", "Porto", "2026-02-01");
    assert_eq!(
        classify_pair(&new, &old, Some(0.93), ClassifyMode::Triage).class,
        ConflictClass::Coexist,
        "an unknown relation is set-valued, so it cannot claim a slot"
    );
    let new = stored("Elias", "born_on", "1980-01-01", "2026-02-01");
    let v = classify_pair(&new, &old, Some(0.93), ClassifyMode::Triage);
    assert_eq!(v.class, ConflictClass::Queue);
    assert_eq!(v.rule, "paraphrase_slot");
    assert_eq!(
        classify_pair(&new, &old, Some(0.70), ClassifyMode::Triage).rule,
        "different_predicate"
    );
}

#[test]
fn two_values_from_one_memory_are_a_real_contradiction() {
    let mut old = stored("Carol", "works_at", "Acme", "2026-01-01");
    let mut new = stored("Carol", "works_at", "Beta Corp", "2026-01-01");
    new.source_memory_id = Uuid::new_v4();
    old.source_memory_id = new.source_memory_id;
    new.valid_from = Some(ts("2026-06-01"));
    let v = triage(&new, &old);
    assert_eq!(v.class, ConflictClass::Queue);
    assert_eq!(v.rule, "same_source_contradiction");
}

#[test]
fn comparable_values_stay_queued_in_triage_but_last_writer_wins_at_write_time() {
    let old = stored("Elias", "works_at", "Acme", "2026-01-01");
    let mut new = stored("Elias", "works_at", "Beta Corp", "2026-01-01");
    new.created_at = old.created_at;
    let v = triage(&new, &old);
    assert_eq!(v.class, ConflictClass::Queue);
    assert_eq!(v.rule, "comparable_values");
    let v = write(&new, &old);
    assert_eq!(v.class, ConflictClass::SupersedeOld);
    assert_eq!(v.rule, "slot_lww");
}

#[test]
fn a_wide_confidence_gap_settles_a_slot_without_a_human() {
    let old = stored("Elias", "works_at", "Acme", "2026-01-01");
    let mut new = stored("Elias", "works_at", "Beta Corp", "2026-01-01");
    new.created_at = old.created_at;
    let mut old_weak = old.clone();
    old_weak.confidence = 0.4;
    let v = triage(&new, &old_weak);
    assert_eq!(v.class, ConflictClass::SupersedeOld);
    assert_eq!(v.rule, "confidence_gap");
    let v = triage(&old_weak, &new);
    assert_eq!(v.class, ConflictClass::SupersedeNew);
    assert_eq!(v.rule, "confidence_gap");
}

#[test]
fn only_the_queue_class_needs_a_human() {
    for (class, expected) in [
        (ConflictClass::Coexist, true),
        (ConflictClass::Duplicate, true),
        (ConflictClass::SupersedeOld, true),
        (ConflictClass::SupersedeNew, true),
        (ConflictClass::Settled, true),
        (ConflictClass::Queue, false),
    ] {
        assert_eq!(Verdict::new(class, "test").is_auto_resolvable(), expected);
    }
}

#[test]
fn a_compiled_fact_is_read_like_a_backlog_not_like_an_assertion() {
    assert_eq!(
        classify_mode_for(SourceKind::CompileLlm),
        ClassifyMode::Triage
    );
    for kind in [
        SourceKind::AgentExtracted,
        SourceKind::UserStated,
        SourceKind::UserConfirmed,
        SourceKind::Import,
    ] {
        assert_eq!(classify_mode_for(kind), ClassifyMode::Write, "{kind:?}");
    }
    let old = stored("Carol", "works_at", "Acme", "2026-01-01");
    let mut new = stored("Carol", "works_at", "Beta Corp", "2026-01-01");
    new.created_at = old.created_at;
    assert_eq!(
        classify_pair(&new, &old, None, classify_mode_for(SourceKind::CompileLlm)).class,
        ConflictClass::Queue
    );
    assert_eq!(compile_resolution("recency_supersede"), "compile_recency");
    assert_eq!(compile_resolution("safety_slot"), "safety_slot");
}

#[test]
fn classification_is_order_consistent() {
    let pairs = [
        (
            stored("Ana", "takes_medication", "metformin", "2026-01-01"),
            stored("Ana", "takes_medication", "rapamycin", "2026-02-01"),
        ),
        (
            stored("Ana", "knows", "Bob", "2026-01-01"),
            stored("Ana", "knows", "Carol", "2026-02-01"),
        ),
        (
            stored("Ana", "works_at", "Acme", "2026-01-01"),
            stored("Bob", "works_at", "Beta", "2026-02-01"),
        ),
    ];
    for (a, b) in pairs {
        let (ab, ba) = (triage(&a, &b), triage(&b, &a));
        assert_eq!(ab.class, ba.class, "{} vs {}", a.object, b.object);
    }
}

#[test]
fn allowed_actions_are_never_narrowed_only_the_advice_is() {
    let new = stored("Ana", "allergic_to", "penicillin", "2026-01-01");
    let old = stored("Ana", "allergic_to", "amoxicillin", "2020-01-01");
    let (nv, ov) = (FactView::of(&new), FactView::of(&old));
    let v = Verdict::new(ConflictClass::Queue, "safety_slot");
    assert_eq!(
        allowed_actions(&v, &nv, &ov),
        &["keep-both", "supersede-old", "reject-new"],
        "a caller decision IS authorizing evidence; permission is not narrowed"
    );
    assert_eq!(allowed_actions(&v, &nv, &ov)[0], "keep-both");
}

// ===========================================================================
// The cessation path
// ===========================================================================

#[test]
fn an_explicit_end_closes_the_open_interval_and_opens_nothing() {
    let taking = stored("Ana", "takes_medication", "metformin 500 mg", "2024-01-01");
    let ended = stated_end(&taking, "2026-06-01");
    let p = plan(&ended, std::slice::from_ref(&taking));
    assert_eq!(p.closes.len(), 1);
    assert_eq!(p.closes[0].fact_id, taking.id);
    assert_eq!(p.closes[0].at, ts("2026-06-01"));
    assert_eq!(p.closes[0].reason, CloseReason::Stopped);
    assert_eq!(p.closes[0].authority, CloseAuthority::ExplicitEnd);
    assert!(p.insert.is_none(), "a cessation is not a second fact");
    assert!(p.queue.is_empty());
}

#[test]
fn an_end_with_no_open_interval_is_reported_not_invented() {
    let unrelated = stored("Ana", "takes_medication", "rapamycin", "2024-01-01");
    let mut ended = observation_of(&stored(
        "Ana",
        "takes_medication",
        "metformin",
        "2026-01-01",
    ));
    ended.state = ObservationState::Ended;
    ended.interval.until = Some(ts("2026-06-01"));
    let p = plan(&ended, &[unrelated]);
    assert!(p.closes.is_empty());
    let inserted = p.insert.expect("the statement leaves a trace");
    assert_eq!(inserted.interval.until, Some(ts("2026-06-01")));
    assert!(inserted.close.is_some(), "the row is born closed");
    assert!(matches!(
        p.notes.first(),
        Some(PlanNote::EndWithoutOpenInterval { .. })
    ));
}

#[test]
fn a_cessation_closes_only_the_member_it_names() {
    let metformin = stored("Ana", "takes_medication", "metformin 500 mg", "2024-01-01");
    let rapamycin = stored("Ana", "takes_medication", "rapamycin 6 mg", "2024-01-01");
    let mut ended = stated_end(&metformin, "2026-06-01");
    ended.interval.until = None;
    let p = plan(&ended, &[metformin.clone(), rapamycin.clone()]);
    assert_eq!(p.closes.len(), 1);
    assert_eq!(
        p.closes[0].fact_id, metformin.id,
        "stopping one drug must not stop the other"
    );
}

/// BLOCKER 1, at the layer that decides it [G1]. The model returns
/// `state: "ended"` for a memory whose text says the opposite. Nothing about
/// the token can be checked from here — so the AUTHORITY it carries is what
/// has to be right.
#[test]
fn an_extracted_cessation_cannot_close_a_safety_interval() {
    let taking = stored("Ana", "takes_medication", "metformin", "2024-01-01");
    let hallucinated = extracted_end(&taking, "2026-06-01");
    let p = plan(&hallucinated, std::slice::from_ref(&taking));
    assert!(
        p.closes.is_empty(),
        "a model's `ended` token must not retire a prescription: {p:?}"
    );
    assert_eq!(p.queue.len(), 1, "the refusal becomes a question: {p:?}");
    assert_eq!(p.queue[0].old_fact_id, taking.id);
    assert_eq!(p.queue[0].verdict.rule, EXTRACTOR_CESSATION);
    assert_eq!(
        p.queue[0].suggested, "requires_context",
        "nobody but the person can settle whether the drug stopped"
    );
    // The claim is still recorded — as a point interval that asserts nothing
    // ongoing — so the caller can see what the extractor said.
    let inserted = p.insert.clone().expect("the claim leaves a trace");
    assert_eq!(inserted.interval.until, Some(ts("2026-06-01")));
    assert_eq!(
        inserted.close.map(|(_, a)| a),
        Some(CloseAuthority::Heuristic {
            rule: EXTRACTOR_CESSATION
        }),
        "the row may not claim an authority the extractor did not have"
    );
    assert!(
        !p.notes
            .iter()
            .any(|n| matches!(n, PlanNote::EndWithoutOpenInterval { .. })),
        "there WAS an open interval; it was refused, not missing: {p:?}"
    );
}

/// The other half of G1: refusing every extracted ending would break the
/// ordinary case the interval model exists for.
#[test]
fn an_extracted_cessation_still_closes_a_non_safety_interval() {
    let lisbon = stored("Bruno", "lives_in", "Lisbon", "2020-01-01");
    let moved = extracted_end(&lisbon, "2026-06-01");
    let p = plan(&moved, std::slice::from_ref(&lisbon));
    assert_eq!(p.closes.len(), 1, "{p:?}");
    assert_eq!(p.closes[0].fact_id, lisbon.id);
    assert_eq!(
        p.closes[0].authority,
        CloseAuthority::Heuristic {
            rule: EXTRACTOR_CESSATION
        },
        "allowed, but recorded for what it is"
    );
    assert_eq!(p.closes[0].resolution_tag, EXTRACTOR_CESSATION);
    assert!(p.queue.is_empty());
}

/// A caller's own triple keeps the standing it always had.
#[test]
fn a_caller_stated_cessation_still_closes_a_safety_interval() {
    let taking = stored("Ana", "takes_medication", "metformin", "2024-01-01");
    for kind in [SourceKind::UserStated, SourceKind::UserConfirmed] {
        let mut ended = stated_end(&taking, "2026-06-01");
        ended.source_kind = kind;
        let p = plan(&ended, std::slice::from_ref(&taking));
        assert_eq!(p.closes.len(), 1, "{kind:?}: {p:?}");
        assert_eq!(p.closes[0].authority, CloseAuthority::ExplicitEnd);
        assert_eq!(p.closes[0].resolution_tag, "explicit_end");
        assert!(p.queue.is_empty());
    }
}

/// The same forgery through the OTHER door: an extracted assertion that
/// carries its own `valid_until` is born closed, and the row it writes must
/// not stamp itself `explicit_end` either.
#[test]
fn an_extracted_window_is_born_closed_under_its_own_authority() {
    let taking = stored("Ana", "takes_medication", "metformin", "2024-01-01");
    let mut windowed = observation_of(&taking);
    windowed.fact_id = Uuid::new_v4();
    windowed.object_norm = "rapamycin".into();
    windowed.member = member_identity("takes_medication", "rapamycin");
    windowed.interval.until = Some(ts("2026-06-01"));
    let p = plan(&windowed, &[]);
    assert_eq!(
        p.insert.and_then(|i| i.close).map(|(_, a)| a),
        Some(CloseAuthority::Heuristic {
            rule: EXTRACTOR_CESSATION
        })
    );
}

// ===========================================================================
// D. Keys and replay
// ===========================================================================

#[test]
fn semantic_key_is_pair_order_independent() {
    let a = stored("Ana", "lives_in", "Lisbon", "2026-01-01");
    let b = stored("Ana", "lives_in", "Porto", "2026-02-01");
    let (av, bv) = (FactView::of(&a), FactView::of(&b));
    assert_eq!(
        SemanticKey::for_pair(&av, &bv).object_pair_hash,
        SemanticKey::for_pair(&bv, &av).object_pair_hash
    );
    assert_eq!(
        SemanticKey::for_pair(&av, &bv).encode(),
        SemanticKey::for_pair(&bv, &av).encode()
    );
}

#[test]
fn semantic_key_changes_with_subject_relation_or_either_object() {
    let base_a = stored("Ana", "lives_in", "Lisbon", "2026-01-01");
    let base_b = stored("Ana", "lives_in", "Porto", "2026-02-01");
    let key =
        |a: &Fact, b: &Fact| SemanticKey::for_pair(&FactView::of(a), &FactView::of(b)).encode();
    let baseline = key(&base_a, &base_b);
    let mut other_subject = base_a.clone();
    other_subject.subject_norm = "bob".into();
    assert_ne!(baseline, key(&other_subject, &base_b));
    let mut other_relation = base_a.clone();
    other_relation.predicate = "works_at".into();
    assert_ne!(baseline, key(&other_relation, &base_b));
    let mut other_object = base_b.clone();
    other_object.object_norm = "braga".into();
    assert_ne!(baseline, key(&base_a, &other_object));
}

#[test]
fn semantic_key_survives_re_derivation() {
    let a = stored("Ana", "lives_in", "Lisbon", "2026-01-01");
    let b = stored("Ana", "lives_in", "Porto", "2026-02-01");
    // Same triples, fresh uuids, fresh source memory — the shape `edit` and
    // `facts backfill` produce, and the whole reason the key is semantic.
    let a2 = stored("Ana", "lives_in", "Lisbon", "2026-03-01");
    let b2 = stored("Ana", "lives_in", "Porto", "2026-04-01");
    assert_ne!(a.id, a2.id);
    assert_eq!(
        SemanticKey::for_pair(&FactView::of(&a), &FactView::of(&b)).encode(),
        SemanticKey::for_pair(&FactView::of(&a2), &FactView::of(&b2)).encode()
    );
    // A synonym spelling keys the same, because the relation is canonical.
    let mut synonym = a2.clone();
    synonym.predicate = "resides_in".into();
    assert_eq!(
        SemanticKey::for_pair(&FactView::of(&a), &FactView::of(&b)).encode(),
        SemanticKey::for_pair(&FactView::of(&synonym), &FactView::of(&b2)).encode()
    );
}

#[test]
fn encode_decode_roundtrip() {
    let key = SemanticKey {
        subject_norm: "ana".into(),
        relation: "lives_in".into(),
        object_pair_hash: keys::object_pair_hash("lisbon", "porto"),
    };
    assert_eq!(SemanticKey::decode(&key.encode()), Some(key));
    // A subject containing the separator cannot round-trip, so it is refused
    // rather than silently re-cut into the wrong three fields.
    let hostile = SemanticKey {
        subject_norm: "ana\u{1f}porto".into(),
        relation: "lives_in".into(),
        object_pair_hash: keys::object_pair_hash("a", "b"),
    };
    assert_eq!(SemanticKey::decode(&hostile.encode()), None);
    assert_eq!(SemanticKey::decode("not a key"), None);
}

fn record(verdict: DecisionVerdict, authority: DecisionAuthority) -> DecisionRecord {
    DecisionRecord {
        id: Uuid::new_v4(),
        key: SemanticKey {
            subject_norm: "ana".into(),
            relation: "lives_in".into(),
            object_pair_hash: keys::object_pair_hash("lisbon", "porto"),
        },
        verdict,
        // A legacy row: no orientation recorded, so it replays as v0.4.0-rc1
        // did. The directional cases pin their own loser.
        loser_object_norm: None,
        authority,
        actor: "cli:default".into(),
        decided_at: ts("2026-05-01"),
        note: None,
        conflict_id: None,
        key_version: SEMANTIC_KEY_VERSION.to_string(),
    }
}

/// The same record, with the value the caller RETIRED written down [G3].
fn record_losing(
    verdict: DecisionVerdict,
    authority: DecisionAuthority,
    loser: &str,
) -> DecisionRecord {
    DecisionRecord {
        loser_object_norm: Some(loser.to_string()),
        ..record(verdict, authority)
    }
}

/// `replay` against a pair spelled (new, old) in that order. The relation is
/// not a parameter any more [G10]: replay treats a medication and an employer
/// alike, so these cases read the same whichever one they name.
fn replayed(prior: &DecisionRecord, current: Verdict, new: &str, old: &str) -> ReplayOutcome {
    replay(prior, current, new, old)
}

fn applied(outcome: ReplayOutcome) -> Option<(ConflictClass, &'static str)> {
    match outcome {
        ReplayOutcome::Applied(v) => Some((v.class, v.rule)),
        _ => None,
    }
}

#[test]
fn a_caller_decision_binds_in_every_direction() {
    let queued = Verdict::new(ConflictClass::Queue, "comparable_values");
    // Keep-both carries no loser because it retired nothing.
    let keep = record(DecisionVerdict::KeepBoth, DecisionAuthority::Caller);
    assert_eq!(
        applied(replayed(&keep, queued, "porto", "lisbon")),
        Some((ConflictClass::Coexist, "replayed_keep_both"))
    );
    let close = record_losing(
        DecisionVerdict::CloseOld,
        DecisionAuthority::Caller,
        "lisbon",
    );
    assert_eq!(
        applied(replayed(&close, queued, "porto", "lisbon")),
        Some((ConflictClass::SupersedeOld, "replayed_close_old"))
    );
    let reject = record_losing(
        DecisionVerdict::RejectNew,
        DecisionAuthority::Caller,
        "porto",
    );
    assert_eq!(
        applied(replayed(&reject, queued, "porto", "lisbon")),
        Some((ConflictClass::SupersedeNew, "replayed_reject_new"))
    );
}

/// G3, at the layer that decides it. The caller retired `lisbon`; the pair
/// then arrives with `lisbon` as the INCOMING value, and the verdict has to
/// travel with the value rather than with the slot.
#[test]
fn a_caller_decision_follows_the_value_it_retired_not_the_pair_order() {
    let queued = Verdict::new(ConflictClass::Queue, "comparable_values");
    let close = record_losing(
        DecisionVerdict::CloseOld,
        DecisionAuthority::Caller,
        "lisbon",
    );
    assert_eq!(
        applied(replayed(&close, queued, "porto", "lisbon")),
        Some((ConflictClass::SupersedeOld, "replayed_close_old")),
        "same orientation as the decision: the old row is still the loser"
    );
    assert_eq!(
        applied(replayed(&close, queued, "lisbon", "porto")),
        Some((ConflictClass::SupersedeNew, "replayed_reject_new")),
        "reversed: the loser arrived as the NEW fact, so the verdict flips"
    );
    // A `reject_new` is the same rule read from the other end.
    let reject = record_losing(
        DecisionVerdict::RejectNew,
        DecisionAuthority::Caller,
        "porto",
    );
    assert_eq!(
        applied(replayed(&reject, queued, "porto", "lisbon")),
        Some((ConflictClass::SupersedeNew, "replayed_reject_new"))
    );
    assert_eq!(
        applied(replayed(&reject, queued, "lisbon", "porto")),
        Some((ConflictClass::SupersedeOld, "replayed_close_old"))
    );
}

/// A key collision, or a re-derivation whose objects changed spelling: the
/// stored answer names a value neither side holds. Guessing a direction here
/// is how a caller's ruling lands on a fact they never saw.
#[test]
fn a_decision_whose_loser_is_on_neither_side_does_not_replay() {
    let queued = Verdict::new(ConflictClass::Queue, "comparable_values");
    let close = record_losing(
        DecisionVerdict::CloseOld,
        DecisionAuthority::Caller,
        "braga",
    );
    assert_eq!(
        replayed(&close, queued, "porto", "lisbon"),
        ReplayOutcome::Unmatched
    );
}

/// The blocker, at the layer that decides it. A decision with no recorded
/// loser and no conflict row left to recover one from knows only that SOMETHING
/// lost, and that is not enough to close an interval with — on ANY relation
/// [G10].
///
/// This test used to assert the opposite for the ordinary half: a legacy row
/// replayed positionally on `lives_in`, on the theory that re-asking about
/// every city a pre-v12 store had settled was the worse trade. Three gate
/// passes disagreed. Pair order is not evidence, and a coin toss that lands
/// wrong retires the value the caller KEPT — Carol's employer as surely as her
/// dose. The queue is where an unanswerable question belongs.
#[test]
fn an_unprovable_orientation_never_retires_anything() {
    let queued = Verdict::new(ConflictClass::Queue, "comparable_values");
    for verdict in [DecisionVerdict::CloseOld, DecisionVerdict::RejectNew] {
        let orphaned = record(verdict, DecisionAuthority::Caller);
        assert_eq!(
            replayed(&orphaned, queued, "metformin 1000 mg", "metformin 500 mg"),
            ReplayOutcome::Directionless,
            "pair order is not evidence of which dose the caller retired"
        );
        assert_eq!(
            replayed(&orphaned, queued, "porto", "lisbon"),
            ReplayOutcome::Directionless,
            "nor of which city — an ordinary relation gets the same refusal"
        );
        assert_eq!(
            replayed(&orphaned, queued, "acme", "beta corp"),
            ReplayOutcome::Directionless,
            "the reported reversal: an employer pair replayed positionally"
        );
    }
}

/// …and the recorded loser is what buys the pair its replay back. The same
/// reversed pair that queues above binds here, in the direction the caller
/// chose rather than the one the re-derivation happened to emit.
#[test]
fn a_recorded_loser_replays_on_a_safety_relation_in_either_order() {
    let queued = Verdict::new(ConflictClass::Queue, "comparable_values");
    let close = record_losing(
        DecisionVerdict::CloseOld,
        DecisionAuthority::Caller,
        "metformin 500 mg",
    );
    assert_eq!(
        applied(replayed(
            &close,
            queued,
            "metformin 1000 mg",
            "metformin 500 mg",
        )),
        Some((ConflictClass::SupersedeOld, "replayed_close_old"))
    );
    assert_eq!(
        applied(replayed(
            &close,
            queued,
            "metformin 500 mg",
            "metformin 1000 mg",
        )),
        Some((ConflictClass::SupersedeNew, "replayed_reject_new")),
        "reversed derivation, same retired dose"
    );
}

#[test]
fn a_machine_close_old_never_replays() {
    let queued = Verdict::new(ConflictClass::Queue, "comparable_values");
    for verdict in [DecisionVerdict::CloseOld, DecisionVerdict::RejectNew] {
        assert_eq!(
            replayed(
                &record_losing(verdict, DecisionAuthority::Machine, "lisbon"),
                queued,
                "porto",
                "lisbon"
            ),
            ReplayOutcome::NotBinding,
            "a stale machine supersession must not retire a fact a second time"
        );
    }
}

#[test]
fn a_machine_keep_both_does_replay() {
    let queued = Verdict::new(ConflictClass::Queue, "comparable_values");
    let v = replayed(
        &record(DecisionVerdict::KeepBoth, DecisionAuthority::Machine),
        queued,
        "porto",
        "lisbon",
    );
    assert_eq!(
        applied(v).map(|(class, _)| class),
        Some(ConflictClass::Coexist)
    );
}

#[test]
fn a_void_decision_is_history_not_a_verdict() {
    let queued = Verdict::new(ConflictClass::Queue, "comparable_values");
    for authority in [DecisionAuthority::Caller, DecisionAuthority::Machine] {
        assert_eq!(
            replayed(
                &record(DecisionVerdict::Void, authority),
                queued,
                "porto",
                "lisbon"
            ),
            ReplayOutcome::NotBinding
        );
    }
}

/// The whole point of move 3: the second derivation does not re-ask.
#[test]
fn a_replayed_keep_both_suppresses_the_question() {
    let old = stored("Elias", "works_at", "Acme", "2026-01-01");
    let mut new = stored("Elias", "works_at", "Beta Corp", "2026-01-01");
    new.created_at = old.created_at;
    new.source_memory_id = old.source_memory_id;
    // Same source memory → `same_source_contradiction`, a genuine question.
    let observation = observation_of(&new);
    let p = plan(&observation, std::slice::from_ref(&old));
    assert_eq!(p.queue.len(), 1, "without a decision it is a question");

    let decided: HashMap<String, DecisionRecord> = [(
        SemanticKey::for_pair(&FactView::of(&new), &FactView::of(&old)).encode(),
        record(DecisionVerdict::KeepBoth, DecisionAuthority::Caller),
    )]
    .into_iter()
    .collect();
    let prior = |k: &SemanticKey| Ok(decided.get(&k.encode()).cloned());
    let p = plan_write(&observation, &[old], &[], ClassifyMode::Write, &prior).unwrap();
    assert!(p.queue.is_empty(), "the caller already answered this");
    assert_eq!(p.replays.len(), 1);
    assert_eq!(p.replays[0].verdict, DecisionVerdict::KeepBoth);
    assert!(matches!(
        p.notes.first(),
        Some(PlanNote::ReplayApplied { .. })
    ));
}

#[test]
fn a_caller_close_old_reapplies_silently_on_a_safety_relation() {
    let old = stored("Ana", "takes_medication", "metformin 500 mg", "2024-01-01");
    let new = stored("Ana", "takes_medication", "metformin 1000 mg", "2026-01-01");
    let observation = observation_of(&new);
    // Un-replayed, a safety dose change is a question.
    let p = plan(&observation, std::slice::from_ref(&old));
    assert_eq!(p.queue.len(), 1);

    let key = SemanticKey::for_pair(&FactView::of(&new), &FactView::of(&old)).encode();
    let decided: HashMap<String, DecisionRecord> = [(
        key.clone(),
        record_losing(
            DecisionVerdict::CloseOld,
            DecisionAuthority::Caller,
            "metformin 500 mg",
        ),
    )]
    .into_iter()
    .collect();
    let prior = |k: &SemanticKey| Ok(decided.get(&k.encode()).cloned());
    let p = plan_write(
        &observation,
        std::slice::from_ref(&old),
        &[],
        ClassifyMode::Write,
        &prior,
    )
    .unwrap();
    assert!(p.queue.is_empty(), "the same question, asked twice");
    assert_eq!(p.closes.len(), 1);
    assert_eq!(p.closes[0].fact_id, old.id);
    assert!(
        matches!(p.closes[0].authority, CloseAuthority::ReplayedCaller { .. }),
        "a replayed CALLER decision is what entitles a safety close"
    );

    // The SAME ruling with no recorded loser and no conflict row left to
    // recover one from closes nothing. Pair order is not a proof of who lost,
    // and on a dose it is the difference between 500 mg and 1000 mg.
    let orphaned: HashMap<String, DecisionRecord> = [(
        key,
        record(DecisionVerdict::CloseOld, DecisionAuthority::Caller),
    )]
    .into_iter()
    .collect();
    let prior = |k: &SemanticKey| Ok(orphaned.get(&k.encode()).cloned());
    let p = plan_write(
        &observation,
        std::slice::from_ref(&old),
        &[],
        ClassifyMode::Write,
        &prior,
    )
    .unwrap();
    assert!(p.closes.is_empty());
    assert!(p.replays.is_empty());
    assert_eq!(p.queue.len(), 1);
    assert_eq!(p.queue[0].verdict.rule, "replay_orientation_unknown");
}

/// BLOCKER 3, as the gate reproduced it. Carol is queued on Acme vs Beta
/// Corp, the caller answers `supersede-old` (Acme loses, Beta stands), and the
/// text is then re-derived with the two employers named in the other order.
/// Directionless replay mapped "close the old one" onto Beta and left Acme
/// open — the exact opposite of what the caller decided.
#[test]
fn a_replayed_decision_survives_the_pair_arriving_reversed() {
    let acme = stored("Carol", "works_at", "Acme", "2026-01-01");
    let mut beta = stored("Carol", "works_at", "Beta Corp", "2026-01-01");
    beta.source_memory_id = acme.source_memory_id;
    let decided: HashMap<String, DecisionRecord> = [(
        SemanticKey::for_pair(&FactView::of(&beta), &FactView::of(&acme)).encode(),
        record_losing(DecisionVerdict::CloseOld, DecisionAuthority::Caller, "acme"),
    )]
    .into_iter()
    .collect();
    let prior = |k: &SemanticKey| Ok(decided.get(&k.encode()).cloned());

    // Re-derivation, reversed: Beta Corp is stored first, so ACME is now the
    // incoming fact and Beta is "the old one".
    let observation = observation_of(&acme);
    let p = plan_write(
        &observation,
        std::slice::from_ref(&beta),
        &[],
        ClassifyMode::Write,
        &prior,
    )
    .unwrap();
    assert_eq!(p.closes.len(), 1, "{p:?}");
    assert_eq!(
        p.closes[0].fact_id, acme.id,
        "the caller retired Acme; a reversed re-derivation must not retire Beta instead"
    );
    assert_eq!(p.replays.len(), 1);
    assert!(p.queue.is_empty());
}

/// …and the same ledger row, on the orientation it was taken in, still does
/// what it always did.
#[test]
fn a_replayed_decision_keeps_its_meaning_when_the_pair_is_not_reversed() {
    let acme = stored("Carol", "works_at", "Acme", "2026-01-01");
    let mut beta = stored("Carol", "works_at", "Beta Corp", "2026-01-01");
    beta.source_memory_id = acme.source_memory_id;
    let decided: HashMap<String, DecisionRecord> = [(
        SemanticKey::for_pair(&FactView::of(&beta), &FactView::of(&acme)).encode(),
        record_losing(DecisionVerdict::CloseOld, DecisionAuthority::Caller, "acme"),
    )]
    .into_iter()
    .collect();
    let prior = |k: &SemanticKey| Ok(decided.get(&k.encode()).cloned());
    let p = plan_write(
        &observation_of(&beta),
        std::slice::from_ref(&acme),
        &[],
        ClassifyMode::Write,
        &prior,
    )
    .unwrap();
    assert_eq!(p.closes.len(), 1, "{p:?}");
    assert_eq!(p.closes[0].fact_id, acme.id);
}

/// BLOCKER 3 in its third form [G10]: the same Carol pair, but the ledger row
/// carries no loser and no conflict row survives to recover one from — a
/// decision taken by a v11 binary whose queue row a `kb delete` cascaded away.
/// `works_at` is ORDINARY, and that used to be licence to replay positionally;
/// the reversed re-derivation then closed Beta Corp, the employer the caller
/// had kept. It asks instead.
#[test]
fn an_unoriented_ordinary_decision_queues_rather_than_guessing_a_side() {
    let acme = stored("Carol", "works_at", "Acme", "2026-01-01");
    let mut beta = stored("Carol", "works_at", "Beta Corp", "2026-01-01");
    beta.source_memory_id = acme.source_memory_id;
    let orphaned: HashMap<String, DecisionRecord> = [(
        SemanticKey::for_pair(&FactView::of(&beta), &FactView::of(&acme)).encode(),
        record(DecisionVerdict::CloseOld, DecisionAuthority::Caller),
    )]
    .into_iter()
    .collect();
    let prior = |k: &SemanticKey| Ok(orphaned.get(&k.encode()).cloned());
    // Reversed: Beta Corp is the stored side, so "close the old one" would
    // land on Beta.
    let p = plan_write(
        &observation_of(&acme),
        std::slice::from_ref(&beta),
        &[],
        ClassifyMode::Write,
        &prior,
    )
    .unwrap();
    assert!(p.closes.is_empty(), "{p:?}");
    assert!(p.replays.is_empty());
    assert_eq!(p.queue.len(), 1);
    assert_eq!(p.queue[0].verdict.rule, "replay_orientation_unknown");
}

/// A stored answer that names neither value is not an answer to THIS pair, so
/// the classifier's own close is withdrawn and the caller is asked.
#[test]
fn an_unmappable_decision_queues_instead_of_closing() {
    let london = stored("Elias", "lives_in", "London", "2015-06-01");
    let lisbon = stored("Elias", "lives_in", "Lisbon", "2024-02-01");
    let decided: HashMap<String, DecisionRecord> = [(
        SemanticKey::for_pair(&FactView::of(&lisbon), &FactView::of(&london)).encode(),
        record_losing(
            DecisionVerdict::CloseOld,
            DecisionAuthority::Caller,
            "braga",
        ),
    )]
    .into_iter()
    .collect();
    let prior = |k: &SemanticKey| Ok(decided.get(&k.encode()).cloned());
    let p = plan_write(
        &observation_of(&lisbon),
        std::slice::from_ref(&london),
        &[],
        ClassifyMode::Write,
        &prior,
    )
    .unwrap();
    assert!(p.closes.is_empty(), "{p:?}");
    assert!(p.replays.is_empty(), "nothing was replayed");
    assert_eq!(p.queue.len(), 1);
    assert_eq!(p.queue[0].verdict.rule, "replay_loser_unmatched");
    assert_eq!(p.queue[0].suggested, "requires_context");
}

#[test]
fn a_failed_decision_lookup_aborts_the_write() {
    let old = stored("Elias", "works_at", "Acme", "2026-01-01");
    let mut new = stored("Elias", "works_at", "Beta Corp", "2026-01-01");
    new.source_memory_id = old.source_memory_id;
    let angry = |_: &SemanticKey| Err(StorageError::Busy("database is locked".into()));
    let err = plan_write(
        &observation_of(&new),
        &[old],
        &[],
        ClassifyMode::Write,
        &angry,
    );
    assert!(
        err.is_err(),
        "reading a busy database as `nobody decided` re-asks a settled safety question"
    );
}

// ===========================================================================
// The drain
// ===========================================================================

#[test]
fn the_drain_resolves_what_the_new_rules_would_never_have_queued() {
    let never_settled = |_: Uuid| false;
    // Two different medications: the dominant class in the 951.
    let a = stored("Ana", "takes_medication", "metformin 500 mg", "2024-01-01");
    let b = stored("Ana", "takes_medication", "rapamycin 6 mg", "2024-02-01");
    let v = triage_verdict(&b, &a, None, &never_settled);
    assert_eq!(v.class, ConflictClass::Coexist);
    assert!(v.is_auto_resolvable());
    assert_eq!(triage_decision(&v), DecisionVerdict::KeepBoth);

    // A dose change stays queued, with the honest advice.
    let c = stored("Ana", "takes_medication", "metformin 1000 mg", "2026-01-01");
    let v = triage_verdict(&c, &a, None, &never_settled);
    assert_eq!(v.class, ConflictClass::Queue);
    assert_eq!(triage_decision(&v), DecisionVerdict::Void);

    // A row whose fact an earlier row in the same sweep already closed.
    let old = stored("Elias", "lives_in", "London", "2015-01-01");
    let new = stored("Elias", "lives_in", "Lisbon", "2024-01-01");
    let closed_id = old.id;
    let v = triage_verdict(&new, &old, None, &|id| id == closed_id);
    assert_eq!(v.class, ConflictClass::Settled);
    assert_eq!(triage_resolution(&v), "already_settled");
    // …and un-settled it is an ordinary recency close.
    let v = triage_verdict(&new, &old, None, &never_settled);
    assert_eq!(triage_resolution(&v), "triage_recency");
    assert_eq!(triage_decision(&v), DecisionVerdict::CloseOld);
}

/// A backlog shaped like the one the audit found — 951 open rows, dominated by
/// second values of set relations and by pairs whose loser was already retired
/// — and what the new rules do to each class (spec §4.1 Phase 2).
///
/// This is the drain's claim written as a table rather than as a number: every
/// class that should leave the queue leaves it, and every class that is a
/// genuine question stays. The count is not the point; the DISPOSITION is.
#[test]
fn the_drain_disposes_of_every_class_in_the_backlog_table() {
    let never_settled = |_: Uuid| false;
    let mut retired = stored("Ana", "allergic_to", "penicillin", "2020-01-01");
    retired.superseded_by = Some(Uuid::new_v4());

    let mut same_memo_a = stored("Carol", "works_at", "Acme", "2026-01-01");
    let same_memo_b = stored("Carol", "works_at", "Beta Corp", "2026-01-01");
    same_memo_a.source_memory_id = same_memo_b.source_memory_id;

    // (name, new, old, expected class, expected rule)
    let backlog: Vec<(&str, Fact, Fact, ConflictClass, &str)> = vec![
        (
            "different medications on one person",
            stored("Ana", "takes_medication", "rapamycin 6 mg", "2026-02-01"),
            stored("Ana", "takes_medication", "metformin 500 mg", "2026-01-01"),
            ConflictClass::Coexist,
            "safety_set_coexist",
        ),
        (
            "different allergies on one person",
            stored("Ana", "allergic_to", "amoxicillin", "2026-01-01"),
            stored("Ana", "allergic_to", "penicillin", "2020-01-01"),
            ConflictClass::Coexist,
            "safety_set_coexist",
        ),
        (
            "a pair whose loser was already retired",
            stored("Ana", "allergic_to", "amoxicillin", "2026-01-01"),
            retired,
            ConflictClass::Settled,
            "already_settled",
        ),
        (
            "single-valued values that never overlapped",
            stored("Elias", "lives_in", "Lisbon", "2024-02-01"),
            closed(
                stored("Elias", "lives_in", "London", "2015-06-01"),
                "2020-01-01",
            ),
            ConflictClass::Coexist,
            "disjoint_intervals",
        ),
        (
            "a second value of a set relation",
            stored("Elias", "knows", "Carol", "2026-02-01"),
            stored("Elias", "knows", "Alice", "2026-01-01"),
            ConflictClass::Coexist,
            "multi_valued",
        ),
        (
            "two people, opposite values",
            stored("Bob", "lives_in", "Porto", "2026-02-01"),
            stored("Alice", "lives_in", "Berlin", "2026-01-01"),
            ConflictClass::Coexist,
            "different_subject",
        ),
        (
            "two attributes of one person",
            stored("Elias", "has_role", "CEO", "2026-02-01"),
            stored("Elias", "lives_in", "Lisbon", "2026-01-01"),
            ConflictClass::Coexist,
            "different_predicate",
        ),
        // …and the residue, which is what a review flow is FOR.
        (
            "same medication, different dose",
            stored("Ana", "takes_medication", "metformin 1000 mg", "2026-06-01"),
            stored("Ana", "takes_medication", "metformin 500 mg", "2026-01-01"),
            ConflictClass::Queue,
            "safety_member_change",
        ),
        (
            "two employers from one memo",
            same_memo_b.clone(),
            same_memo_a.clone(),
            ConflictClass::Queue,
            "same_source_contradiction",
        ),
        (
            "two birthdays",
            stored("Ana", "birthday", "1991-04-12", "2026-01-01"),
            stored("Ana", "birthday", "1990-04-12", "2020-01-01"),
            ConflictClass::Queue,
            "immutable_attribute_disagreement",
        ),
    ];

    let mut resolved = 0usize;
    for (name, new, old, class, rule) in &backlog {
        let v = triage_verdict(new, old, None, &never_settled);
        assert_eq!(v.class, *class, "{name}: {:?}/{}", v.class, v.rule);
        assert_eq!(v.rule, *rule, "{name}");
        if v.is_auto_resolvable() {
            resolved += 1;
            // A coexistence is an ANSWER and binds a later write; a `Settled`
            // pair answers nothing — its question stopped existing — so it
            // records `void` and never replays. Both are terminal, and the
            // difference is exactly what stops a cascaded row from binding.
            let decided = triage_decision(&v);
            if v.class == ConflictClass::Coexist {
                assert_eq!(decided, DecisionVerdict::KeepBoth, "{name}");
            } else {
                assert_eq!(decided, DecisionVerdict::Void, "{name}");
            }
        }
    }
    assert_eq!(
        resolved, 7,
        "seven of ten classes leave the queue unattended"
    );
    // Not one of them closed a safety interval.
    for (name, new, old, _, _) in &backlog {
        let v = triage_verdict(new, old, None, &never_settled);
        let safety = FactView::of(new).is_safety() || FactView::of(old).is_safety();
        assert!(
            !safety || triage_decision(&v) != DecisionVerdict::CloseOld,
            "{name} would have retired a safety fact"
        );
    }
}

#[test]
fn the_drain_never_closes_a_safety_interval() {
    let never_settled = |_: Uuid| false;
    for (a, b) in [
        ("penicillin", "amoxicillin"),
        ("metformin 500 mg", "metformin 1000 mg"),
    ] {
        let old = stored("Ana", "allergic_to", a, "2020-01-01");
        let new = stored("Ana", "allergic_to", b, "2026-01-01");
        let v = triage_verdict(&new, &old, None, &never_settled);
        assert!(
            !matches!(
                v.class,
                ConflictClass::SupersedeOld | ConflictClass::SupersedeNew
            ),
            "{a}/{b} → {:?}",
            v.class
        );
        assert_ne!(triage_decision(&v), DecisionVerdict::CloseOld);
    }
}

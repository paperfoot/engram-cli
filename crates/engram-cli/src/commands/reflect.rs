//! `engram reflect` — read the journal, propose lessons.
//!
//! The decision journal already records every time a caller overruled the
//! machine, every correction made minutes after a write, every slot that keeps
//! churning, and every source whose extracted facts were retracted. Nobody
//! read it. Reflection is the pass that turns that record into procedural
//! memory: deterministic proposers over the event window, each with a stable
//! `rule` id and a templated sentence.
//!
//! **Deterministic, offline, and unpaid — by construction, not as a
//! fallback.** There is no model in this pipeline: every lesson it writes comes
//! from a rule you can read in this file, so the output is reproducible, costs
//! nothing, and cannot hallucinate a lesson nobody's work supports. An LLM
//! rewrite pass over the templated sentences is a v0.5 question (it can only
//! make the prose nicer; it cannot make the evidence better).
//!
//! Everything it writes lands at `status = "proposed"` with NO memory twin, so
//! `recall` never surfaces an unreviewed machine sentence as if a person had
//! said it. `engram lessons confirm` is what promotes one.
//!
//! **Two proposers are not pure over the window, and say so.** A memory
//! revised today may have been written months ago, below any window floor, so
//! `correction_within_window` and `retraction_after_extraction` resolve their
//! antecedent by subject through the `Antecedents` seam. A proposer that
//! silently drops every lesson whose cause predates the window is a proposer
//! that only works on a store younger than one window.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use engram_core::events::EventKind;
use engram_core::relations::canonical_relation;
use engram_core::types::Fact;
use engram_storage::journal::EventRow;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::context::AppContext;
use crate::error::CliError;
use crate::output::{print_success, Listing, Metadata};

/// A correction this close to the write is about the write, not about the
/// world changing.
const FRESH_CORRECTION_MINUTES: i64 = 15;
/// How many closes on one slot inside a window make it churn rather than
/// change.
const CHURN_THRESHOLD: usize = 3;

/// The six kinds a reflection pass reads. Four are triggers — what the work
/// taught us — and two are ANTECEDENTS that a trigger event does not carry.
const READ_KINDS: [EventKind; 6] = [
    EventKind::DecisionRecorded,
    EventKind::MemoryRevised,
    EventKind::MemoryRetracted,
    EventKind::FactIntervalClosed,
    EventKind::MemoryWritten,
    EventKind::DerivationRan,
];

pub struct ReflectArgs {
    pub kb: String,
    pub since: Option<String>,
    pub window: usize,
    pub dry_run: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LessonCandidate {
    pub rule: &'static str,
    pub trigger: String,
    pub summary: String,
    pub evidence: Vec<Uuid>,
    pub confidence: f32,
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct ReflectStats {
    pub events_scanned: usize,
    pub candidates: usize,
    pub lessons_written: usize,
    pub lessons_merged: usize,
    pub high_water_seq: i64,
}

/// The one seam through which a proposer may reach outside its window.
///
/// A trait rather than a `&SqliteStore` so the rules can be unit-tested
/// against a `HashMap` with no database at all — the impurity is one named
/// door, not store access sprayed through four functions.
pub trait Antecedents {
    /// Every event about one subject, at any seq.
    fn events_for_subject(&self, id: Uuid) -> Result<Vec<EventRow>, CliError>;
    /// `fact.interval_closed` carries only `fact_id`; the proposer that groups
    /// closes by slot has to resolve it.
    fn fact_of(&self, fact_id: Uuid) -> Result<Option<Fact>, CliError>;
}

pub struct StoreAntecedents<'a>(pub &'a AppContext);

impl Antecedents for StoreAntecedents<'_> {
    fn events_for_subject(&self, id: Uuid) -> Result<Vec<EventRow>, CliError> {
        Ok(self.0.store.events_for_subject(id)?)
    }
    fn fact_of(&self, fact_id: Uuid) -> Result<Option<Fact>, CliError> {
        Ok(self.0.store.get_fact(fact_id)?)
    }
}

fn payload_uuid(e: &EventRow, key: &str) -> Option<Uuid> {
    e.payload
        .get(key)
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok())
}

fn payload_str<'a>(e: &'a EventRow, key: &str) -> Option<&'a str> {
    e.payload.get(key).and_then(|v| v.as_str())
}

/// A caller decision that disagrees with a machine decision on the same
/// question. The single most valuable thing the journal knows: it is a
/// recorded instance of the store being wrong in a way a person cared about.
///
/// KNOWN NARROWING: both decisions must be inside the scanned window. The
/// machine decision that a caller overrules is written seconds earlier by the
/// same write path, so this holds in practice; a caller who reverses a ruling
/// months later is not seen. Widening it needs a decisions-by-key query, which
/// is storage-journal's to add.
pub fn caller_overruled_machine(window: &[EventRow]) -> Vec<LessonCandidate> {
    let mut machine: HashMap<String, (String, Uuid)> = HashMap::new();
    for e in window {
        if e.kind != EventKind::DecisionRecorded {
            continue;
        }
        if payload_str(e, "authority") == Some("machine") {
            if let (Some(key), Some(verdict)) =
                (payload_str(e, "semantic_key"), payload_str(e, "verdict"))
            {
                // A `void` rules on nothing: it records that a question was
                // merged away, or which judge was consulted on one
                // (`judge:<name>`, fact_write.rs `record_judgement`). Read as
                // a ruling, every caller answer after a failed, stale or
                // safety-refused judge became a false "caller overruled
                // machine" lesson.
                if verdict == "void" {
                    continue;
                }
                machine.insert(key.to_string(), (verdict.to_string(), e.id));
            }
        }
    }
    let mut out = Vec::new();
    for e in window {
        if e.kind != EventKind::DecisionRecorded || payload_str(e, "authority") != Some("caller") {
            continue;
        }
        let (Some(key), Some(verdict)) =
            (payload_str(e, "semantic_key"), payload_str(e, "verdict"))
        else {
            continue;
        };
        let Some((machine_verdict, machine_event)) = machine.get(key) else {
            continue;
        };
        if machine_verdict == verdict {
            continue;
        }
        let (subject, relation) = engram_core::keys::semantic_key_decode(key)
            .map(|(s, r, _)| (s, r))
            .unwrap_or_else(|| ("this subject".to_string(), "unknown".to_string()));
        let relation = canonical_relation(&relation);
        out.push(LessonCandidate {
            rule: "caller_overruled_machine",
            trigger: format!("conflict_{relation}"),
            summary: format!(
                "On `{relation}` for {subject}, the machine ruled `{machine_verdict}` and the \
                 caller ruled `{verdict}`. Surface this slot instead of settling it."
            ),
            evidence: vec![*machine_event, e.id],
            confidence: 0.7,
        });
    }
    out
}

/// A memory corrected minutes after it was written. That is a lesson about
/// HOW it was written, not about the world changing.
pub fn correction_within_window(
    window: &[EventRow],
    ante: &dyn Antecedents,
) -> Result<Vec<LessonCandidate>, CliError> {
    let mut out = Vec::new();
    for e in window {
        if e.kind != EventKind::MemoryRevised {
            continue;
        }
        let Some(memory_id) = payload_uuid(e, "memory_id").or(e.subject_id) else {
            continue;
        };
        // The lookback: the `memory.written` may sit far below the window
        // floor, which is the common case rather than the edge one.
        let written = ante
            .events_for_subject(memory_id)?
            .into_iter()
            .find(|w| w.kind == EventKind::MemoryWritten);
        let Some(written) = written else { continue };
        let gap = e.ts - written.ts;
        if gap.num_minutes() > FRESH_CORRECTION_MINUTES || gap.num_milliseconds() < 0 {
            continue;
        }
        out.push(LessonCandidate {
            rule: "correction_within_window",
            trigger: "fresh_write_correction".to_string(),
            summary: format!(
                "A memory was corrected {} minute(s) after it was written. Re-read what you are \
                 about to store before storing it.",
                gap.num_minutes().max(0)
            ),
            evidence: vec![written.id, e.id],
            confidence: 0.6,
        });
    }
    Ok(out)
}

/// One slot closed over and over inside a window is churn, not change.
pub fn repeat_close(
    window: &[EventRow],
    ante: &dyn Antecedents,
) -> Result<Vec<LessonCandidate>, CliError> {
    let mut by_slot: HashMap<(String, String), Vec<Uuid>> = HashMap::new();
    for e in window {
        if e.kind != EventKind::FactIntervalClosed {
            continue;
        }
        let Some(fact_id) = payload_uuid(e, "fact_id") else {
            continue;
        };
        // The payload carries only `fact_id` — no subject, no relation — so
        // the grouping has to resolve the fact. Widening a binding event
        // schema for one heuristic is the worse trade.
        let Some(fact) = ante.fact_of(fact_id)? else {
            continue;
        };
        let relation = canonical_relation(&fact.predicate);
        by_slot
            .entry((fact.subject_norm.clone(), relation))
            .or_default()
            .push(e.id);
    }
    let mut out: Vec<LessonCandidate> = by_slot
        .into_iter()
        .filter(|(_, events)| events.len() >= CHURN_THRESHOLD)
        .map(|((subject, relation), events)| LessonCandidate {
            rule: "repeat_close",
            trigger: format!("churny_slot_{relation}"),
            summary: format!(
                "`{relation}` for {subject} has been closed {} times in one window. Ask for the \
                 current value instead of inferring it.",
                events.len()
            ),
            evidence: events,
            confidence: 0.6,
        })
        .collect();
    // Deterministic order: a HashMap's iteration order is not an answer.
    out.sort_by(|a, b| {
        a.trigger
            .cmp(&b.trigger)
            .then_with(|| a.summary.cmp(&b.summary))
    });
    Ok(out)
}

/// A retracted memory whose extraction had already produced facts. The source
/// was bad AND the store believed it.
pub fn retraction_after_extraction(
    window: &[EventRow],
    ante: &dyn Antecedents,
) -> Result<Vec<LessonCandidate>, CliError> {
    let mut out = Vec::new();
    for e in window {
        if e.kind != EventKind::MemoryRetracted {
            continue;
        }
        let Some(memory_id) = payload_uuid(e, "memory_id").or(e.subject_id) else {
            continue;
        };
        let derivation = ante
            .events_for_subject(memory_id)?
            .into_iter()
            .find(|d| d.kind == EventKind::DerivationRan);
        let Some(derivation) = derivation else {
            continue;
        };
        let fact_count = derivation
            .payload
            .get("fact_count")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        if fact_count < 1 {
            continue;
        }
        out.push(LessonCandidate {
            rule: "retraction_after_extraction",
            trigger: "bad_extraction_source".to_string(),
            summary: format!(
                "A memory that had already produced {fact_count} extracted fact(s) was retracted. \
                 Check what a source asserts before letting extraction run over it."
            ),
            evidence: vec![derivation.id, e.id],
            confidence: 0.6,
        });
    }
    Ok(out)
}

pub fn propose(
    window: &[EventRow],
    ante: &dyn Antecedents,
) -> Result<Vec<LessonCandidate>, CliError> {
    let mut all = caller_overruled_machine(window);
    all.extend(correction_within_window(window, ante)?);
    all.extend(repeat_close(window, ante)?);
    all.extend(retraction_after_extraction(window, ante)?);
    Ok(all)
}

fn parse_since(raw: &Option<String>) -> Result<Option<DateTime<Utc>>, CliError> {
    let Some(raw) = raw else { return Ok(None) };
    let raw = raw.trim();
    if let Ok(dt) = DateTime::parse_from_rfc3339(raw) {
        return Ok(Some(dt.with_timezone(&Utc)));
    }
    if let Ok(d) = chrono::NaiveDate::parse_from_str(raw, "%Y-%m-%d") {
        return Ok(Some(DateTime::from_naive_utc_and_offset(
            d.and_hms_opt(0, 0, 0).expect("midnight exists"),
            Utc,
        )));
    }
    Err(CliError::BadInput(format!(
        "--since `{raw}` is not a timestamp — pass RFC3339 or a bare date (2026-03-01)"
    )))
}

/// `engram reflect`.
///
/// Synchronous. The daemon's job API is not in this tree, and blocking on it
/// would have shipped nothing; the pipeline is a pure function of the journal
/// window, so running it in the caller's process costs correctness nothing.
/// When daemon-jobs exposes a `reflect` job kind, this body is what it runs.
pub fn run(ctx: &AppContext, args: ReflectArgs) -> Result<(), CliError> {
    let since = parse_since(&args.since)?;
    let window = args.window.max(1);
    // The floor. With `--since`, the seq at that instant minus one, because
    // `events_slice` is exclusive; without it, the tail of the journal —
    // `seq` is AUTOINCREMENT over a table nothing is ever deleted from, so
    // `max - window` is exactly the last `window` events, not an estimate.
    let floor = match since {
        Some(ts) => (ctx.store.seq_at_or_after(ts)? - 1).max(0),
        None => (ctx.store.max_event_seq()? - window as i64).max(0),
    };
    let events = ctx.store.events_since(&READ_KINDS, floor, window)?;
    let high_water = events.last().map(|e| e.seq).unwrap_or(floor);

    let ante = StoreAntecedents(ctx);
    let candidates = propose(&events, &ante)?;

    let mut stats = ReflectStats {
        events_scanned: events.len(),
        candidates: candidates.len(),
        high_water_seq: high_water,
        ..Default::default()
    };
    let at = Utc::now();
    let mut written: Vec<Value> = Vec::new();
    for c in &candidates {
        if args.dry_run {
            written.push(json!({
                "rule": c.rule,
                "trigger": c.trigger,
                "lesson": c.summary,
                "evidence": c.evidence.iter().map(|e| e.to_string()).collect::<Vec<_>>(),
                "status": "candidate",
            }));
            continue;
        }
        let (row, outcome) = crate::commands::lessons::record_proposed(
            ctx,
            &args.kb,
            &crate::commands::lessons::Proposal {
                trigger: &c.trigger,
                lesson: &c.summary,
                rule: c.rule,
                evidence: c.evidence.clone(),
                confidence: c.confidence,
            },
            at,
        )?;
        match outcome {
            engram_storage::LessonUpsert::Inserted => stats.lessons_written += 1,
            engram_storage::LessonUpsert::Merged => stats.lessons_merged += 1,
        }
        written.push(crate::commands::lessons::to_json(&row));
    }

    let mut payload = serde_json::Map::new();
    payload.insert("kb".to_string(), json!(args.kb));
    payload.insert("dry_run".to_string(), json!(args.dry_run));
    payload.insert("events_scanned".to_string(), json!(stats.events_scanned));
    payload.insert("candidates".to_string(), json!(stats.candidates));
    payload.insert("lessons_written".to_string(), json!(stats.lessons_written));
    payload.insert("lessons_merged".to_string(), json!(stats.lessons_merged));
    payload.insert("high_water_seq".to_string(), json!(stats.high_water_seq));
    payload.insert("lessons".to_string(), Value::Array(written.clone()));
    Listing::full(written.len()).attach(&mut payload);

    let mut meta = Metadata::default();
    meta.add("deterministic", true);
    // The resume hint is published rather than remembered: without the
    // daemon's job row there is nowhere durable to keep a high-water mark, and
    // a mark this command invented and forgot would be worse than none.
    meta.add("resume_with", json!({ "since_seq": stats.high_water_seq }));
    print_success(ctx.format, Value::Object(payload), meta, |data| {
        println!(
            "Scanned {} journal events, {} candidate(s)",
            data["events_scanned"].as_i64().unwrap_or(0),
            data["candidates"].as_i64().unwrap_or(0)
        );
        for l in data["lessons"].as_array().cloned().unwrap_or_default() {
            println!(
                "  [{}] {}",
                l["trigger"].as_str().unwrap_or(""),
                l["lesson"].as_str().unwrap_or("")
            );
        }
        if data["dry_run"] == json!(true) {
            println!("(dry run — nothing was written)");
        } else {
            println!(
                "{} new, {} merged. Confirm one with `engram lessons confirm <id>`.",
                data["lessons_written"].as_i64().unwrap_or(0),
                data["lessons_merged"].as_i64().unwrap_or(0)
            );
        }
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use engram_core::keys::{object_pair_hash, semantic_key_encode};

    /// A `HashMap`-backed `Antecedents`: the proposers' one impure seam,
    /// closed with no database at all.
    #[derive(Default)]
    struct FakeAntecedents {
        events: HashMap<Uuid, Vec<EventRow>>,
        facts: HashMap<Uuid, Fact>,
    }

    impl Antecedents for FakeAntecedents {
        fn events_for_subject(&self, id: Uuid) -> Result<Vec<EventRow>, CliError> {
            Ok(self.events.get(&id).cloned().unwrap_or_default())
        }
        fn fact_of(&self, fact_id: Uuid) -> Result<Option<Fact>, CliError> {
            Ok(self.facts.get(&fact_id).cloned())
        }
    }

    fn event(seq: i64, kind: EventKind, ts: DateTime<Utc>, payload: Value) -> EventRow {
        EventRow {
            seq,
            id: Uuid::new_v4(),
            ts,
            actor: "cli:default".to_string(),
            kind,
            subject_id: None,
            kb: Some("default".to_string()),
            diary: Some("default".to_string()),
            payload,
            origin: engram_storage::Origin::Own,
        }
    }

    fn t(minutes: i64) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-07-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
            + chrono::Duration::minutes(minutes)
    }

    fn key(subject: &str, relation: &str) -> String {
        semantic_key_encode(subject, relation, &object_pair_hash("lisbon", "porto"))
    }

    #[test]
    fn a_caller_overruling_the_machine_is_a_lesson() {
        let k = key("ada example", "lives_in");
        let window = vec![
            event(
                1,
                EventKind::DecisionRecorded,
                t(0),
                json!({ "semantic_key": k, "verdict": "close_old", "authority": "machine" }),
            ),
            event(
                2,
                EventKind::DecisionRecorded,
                t(1),
                json!({ "semantic_key": k, "verdict": "keep_both", "authority": "caller" }),
            ),
        ];
        let out = caller_overruled_machine(&window);
        assert_eq!(out.len(), 1, "{out:?}");
        assert_eq!(out[0].rule, "caller_overruled_machine");
        assert_eq!(out[0].trigger, "conflict_lives_in");
        assert_eq!(out[0].evidence.len(), 2, "both sides are the evidence");
        assert!(out[0].summary.contains("ada example"));
    }

    #[test]
    fn a_void_machine_row_is_not_a_ruling() {
        let k = key("ada example", "lives_in");
        let window = vec![
            event(
                1,
                EventKind::DecisionRecorded,
                t(0),
                json!({ "semantic_key": k, "verdict": "void", "authority": "machine" }),
            ),
            event(
                2,
                EventKind::DecisionRecorded,
                t(1),
                json!({ "semantic_key": k, "verdict": "keep_both", "authority": "caller" }),
            ),
        ];
        assert!(caller_overruled_machine(&window).is_empty());
    }

    #[test]
    fn a_caller_agreeing_with_the_machine_teaches_nothing() {
        let k = key("ada example", "lives_in");
        let window = vec![
            event(
                1,
                EventKind::DecisionRecorded,
                t(0),
                json!({ "semantic_key": k, "verdict": "keep_both", "authority": "machine" }),
            ),
            event(
                2,
                EventKind::DecisionRecorded,
                t(1),
                json!({ "semantic_key": k, "verdict": "keep_both", "authority": "caller" }),
            ),
        ];
        assert!(caller_overruled_machine(&window).is_empty());
    }

    /// The antecedent is BELOW the window floor — the common case, not the
    /// edge one. A proposer pure over the window alone fails this and works
    /// only on a store younger than one window.
    #[test]
    fn a_correction_fires_even_when_the_write_predates_the_window() {
        let memory_id = Uuid::new_v4();
        let written = event(
            1,
            EventKind::MemoryWritten,
            t(0),
            json!({ "content_hash": "abc" }),
        );
        let revised = event(
            9_000,
            EventKind::MemoryRevised,
            t(3),
            json!({ "memory_id": memory_id.to_string() }),
        );
        let mut ante = FakeAntecedents::default();
        ante.events.insert(memory_id, vec![written.clone()]);

        // The window contains ONLY the revision.
        let out = correction_within_window(&[revised], &ante).unwrap();
        assert_eq!(out.len(), 1, "{out:?}");
        assert_eq!(out[0].trigger, "fresh_write_correction");
        assert_eq!(out[0].evidence[0], written.id);
    }

    #[test]
    fn a_correction_months_later_is_the_world_changing_not_a_lesson() {
        let memory_id = Uuid::new_v4();
        let written = event(1, EventKind::MemoryWritten, t(0), json!({}));
        let revised = event(
            2,
            EventKind::MemoryRevised,
            t(60 * 24 * 30),
            json!({ "memory_id": memory_id.to_string() }),
        );
        let mut ante = FakeAntecedents::default();
        ante.events.insert(memory_id, vec![written]);
        assert!(correction_within_window(&[revised], &ante)
            .unwrap()
            .is_empty());
    }

    fn fact(subject: &str, predicate: &str) -> Fact {
        Fact {
            id: Uuid::new_v4(),
            source_memory_id: Uuid::new_v4(),
            subject: subject.to_string(),
            subject_norm: subject.to_lowercase(),
            predicate: predicate.to_string(),
            object: "somewhere".to_string(),
            object_norm: "somewhere".to_string(),
            confidence: 1.0,
            created_at: t(0),
            superseded_by: None,
            superseded_at: None,
            diary: "default".to_string(),
            fact_type: engram_core::types::FactType::Assertion,
            valid_from: None,
            valid_until: None,
            resolution: None,
            source_kind: engram_core::types::SourceKind::UserStated,
            confirmations: 0,
            closed_by: None,
            close_reason: None,
            close_authority: None,
            kb: "default".to_string(),
        }
    }

    #[test]
    fn three_closes_on_one_slot_is_churn() {
        let mut ante = FakeAntecedents::default();
        let mut window = Vec::new();
        for i in 0..CHURN_THRESHOLD {
            let f = fact("Ada Example", "lives_in");
            let id = f.id;
            ante.facts.insert(id, f);
            window.push(event(
                i as i64,
                EventKind::FactIntervalClosed,
                t(i as i64),
                json!({ "fact_id": id.to_string() }),
            ));
        }
        let out = repeat_close(&window, &ante).unwrap();
        assert_eq!(out.len(), 1, "{out:?}");
        assert_eq!(out[0].trigger, "churny_slot_lives_in");
        assert_eq!(out[0].evidence.len(), CHURN_THRESHOLD);

        // Two is not yet a pattern.
        let out = repeat_close(&window[..2], &ante).unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn a_retraction_after_a_productive_extraction_is_a_lesson() {
        let memory_id = Uuid::new_v4();
        let derivation = event(
            1,
            EventKind::DerivationRan,
            t(0),
            json!({ "memory_id": memory_id.to_string(), "fact_count": 3 }),
        );
        let retracted = event(
            2,
            EventKind::MemoryRetracted,
            t(10),
            json!({ "memory_id": memory_id.to_string() }),
        );
        let mut ante = FakeAntecedents::default();
        ante.events.insert(memory_id, vec![derivation]);
        let out = retraction_after_extraction(&[retracted], &ante).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].trigger, "bad_extraction_source");

        // A retraction of a memory that produced nothing teaches nothing.
        let barren = Uuid::new_v4();
        let mut ante = FakeAntecedents::default();
        ante.events.insert(
            barren,
            vec![event(
                1,
                EventKind::DerivationRan,
                t(0),
                json!({ "memory_id": barren.to_string(), "fact_count": 0 }),
            )],
        );
        let retracted = event(
            2,
            EventKind::MemoryRetracted,
            t(1),
            json!({ "memory_id": barren.to_string() }),
        );
        assert!(retraction_after_extraction(&[retracted], &ante)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn since_accepts_a_bare_date_and_refuses_prose() {
        assert!(parse_since(&Some("2026-03-01".into())).unwrap().is_some());
        assert_eq!(
            parse_since(&Some("yesterday".into()))
                .unwrap_err()
                .exit_code(),
            3
        );
    }
}

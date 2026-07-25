//! Write-path v2 orchestration: semantic supersession + cooperative
//! adjudication.
//!
//! Per extracted fact:
//! 1. canonicalize the predicate (synonym map) and the subject (alias/fuzzy
//!    cascade against known subjects), then normalize;
//! 2. exact path — an active fact with the same (subject_norm, predicate):
//!    same object → dedup (confirmation++); different object → slot policy:
//!    last-writer-wins supersedes, await-confirmation queues (both active);
//!    non-slot exact conflicts supersede (same question, new answer);
//! 3. semantic path — the embedding novelty gate classifies against all
//!    active facts: duplicate → confirmation++; novel → insert; uncertain →
//!    resolve by autonomy mode: `full` asks this system's LLM, `assisted`
//!    (default) inserts the new fact as *contested* and queues the conflict
//!    for the calling agent, `strict` always queues.
//!
//! The caller is a frontier agent with conversation context this store
//! cannot see — `assisted` mode hands it the adjudication instead of paying
//! for a worse-informed LLM call here.

use crate::context::AppContext;
use crate::error::CliError;
use chrono::{DateTime, Utc};
use engram_core::temporal::parse_moment;
use engram_core::types::{Fact, FactType, SourceKind};
use engram_embed::{Embedder, TaskMode};
use engram_graph::facts::{
    adjudicate, canonical_predicate, date_valued_predicate, normalize, slot_policy,
    AdjudicationAction, ExtractedFact, SlotPolicy,
};
use engram_graph::gate::{novelty_gate, GateDecision};
use engram_llm::ChatLlm;
use serde_json::json;
use uuid::Uuid;

/// How much authority this process has over conflict resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Autonomy {
    /// Adjudicate uncertain conflicts with our own LLM call.
    Full,
    /// Queue uncertain conflicts for the calling agent (default).
    #[default]
    Assisted,
    /// Never auto-supersede anything; queue every conflict.
    Strict,
}

impl Autonomy {
    pub fn parse(s: &str) -> Result<Autonomy, CliError> {
        match s {
            "full" => Ok(Autonomy::Full),
            "assisted" => Ok(Autonomy::Assisted),
            "strict" => Ok(Autonomy::Strict),
            other => Err(CliError::BadInput(format!(
                "unknown autonomy mode '{other}' (expected full|assisted|strict)"
            ))),
        }
    }
}

/// Aggregated outcome of writing one batch of extracted facts.
#[derive(Debug, Default)]
pub struct FactWriteOutcome {
    pub added: usize,
    pub deduped: usize,
    /// Resolved supersessions (old value → new value), for the envelope.
    pub conflicts: Vec<serde_json::Value>,
    /// Unresolved conflicts queued for the caller (`adjudication_needed`).
    pub queued: Vec<serde_json::Value>,
}

/// Thresholds for the novelty gate, env-overridable so they can be tuned on
/// the bench harness without a rebuild.
fn gate_thresholds() -> (f32, f32) {
    let dup = std::env::var("ENGRAM_FACT_DUP_THRESHOLD")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(engram_graph::gate::DEFAULT_DUP_THRESHOLD);
    let novel = std::env::var("ENGRAM_FACT_NOVEL_THRESHOLD")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(engram_graph::gate::DEFAULT_NOVEL_THRESHOLD);
    (dup, novel)
}

/// When this fact became true, in the bi-temporal sense the v6 schema models.
/// Precedence, most specific first:
/// 1. an explicit `valid_from` from the extractor ("since March 2026");
/// 2. the object of a date-valued predicate — `born_on 1990-04-12` was true
///    in 1990, whatever today's date is;
/// 3. the parent memory's `--event-time` — facts learned about an event
///    inherit that event's time;
/// 4. now, so the temporal model is never left NULL.
fn resolve_valid_from(
    ef: &ExtractedFact,
    predicate: &str,
    memory_event_time: Option<DateTime<Utc>>,
) -> Option<DateTime<Utc>> {
    ef.valid_from
        .as_deref()
        .and_then(parse_moment)
        .or_else(|| {
            date_valued_predicate(predicate)
                .then(|| parse_moment(&ef.object))
                .flatten()
        })
        .or(memory_event_time)
        .or_else(|| Some(Utc::now()))
}

/// Resolve a raw subject against known fact subjects in (diary, kb):
/// exact → entity-alias table → fuzzy (token-sorted Jaro-Winkler). Returns
/// the canonical display form and its norm. Falls back to the raw subject.
pub fn canonicalize_subject(
    ctx: &AppContext,
    diary: &str,
    kb: &str,
    raw_subject: &str,
) -> (String, String) {
    let raw_norm = normalize(raw_subject);
    if raw_norm.is_empty() {
        return (raw_subject.to_string(), raw_norm);
    }
    // Exact: this subject_norm already exists — nothing to resolve.
    if let Ok(existing) =
        ctx.store
            .list_facts_by_subject_scoped(&raw_norm, Some(diary), Some(kb), true)
    {
        if !existing.is_empty() {
            return (raw_subject.to_string(), raw_norm);
        }
    }
    // Alias table: a compile pass may have recorded this name as an alias.
    if let Ok(Some(canonical)) = ctx.store.resolve_entity_alias(kb, &raw_norm) {
        let canon_norm = normalize(&canonical);
        if !canon_norm.is_empty() {
            return (canonical, canon_norm);
        }
    }
    // Fuzzy: token-sorted Jaro-Winkler against known subjects catches
    // "Robert Smith" vs "Smith, Robert" and small spelling drift.
    if let Ok(subjects) = ctx.store.distinct_fact_subjects(diary, kb) {
        let target = token_sort(&raw_norm);
        let mut best: Option<(f64, String, String)> = None;
        for (display, norm) in subjects {
            let score = strsim::jaro_winkler(&target, &token_sort(&norm));
            if score >= 0.92 && best.as_ref().map(|(b, _, _)| score > *b).unwrap_or(true) {
                best = Some((score, display, norm));
            }
        }
        if let Some((_, display, norm)) = best {
            return (display, norm);
        }
    }
    (raw_subject.to_string(), raw_norm)
}

fn token_sort(s: &str) -> String {
    let mut tokens: Vec<&str> = s
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .collect();
    tokens.sort_unstable();
    tokens.join(" ")
}

fn fact_text(subject: &str, predicate: &str, object: &str) -> String {
    format!("{subject} {predicate} {object}")
}

fn fact_summary(f: &Fact) -> serde_json::Value {
    json!({
        "id": f.id.to_string(),
        "subject": f.subject,
        "predicate": f.predicate,
        "object": f.object,
        "fact_type": f.fact_type.as_str(),
        "created_at": f.created_at.to_rfc3339(),
    })
}

/// Everything a batch of facts from one memory shares. Bundled so the write
/// path keeps one honest signature as the fact layer grows.
pub struct FactWriteCtx<'a> {
    /// Memory the facts were derived from (provenance + backfill idempotency).
    pub memory_id: Uuid,
    pub diary: &'a str,
    pub kb: &'a str,
    /// The memory's event time, inherited by facts with no date of their own.
    pub event_time: Option<DateTime<Utc>>,
    /// Powers the novelty gate. `None` = gate off, exact path only.
    pub embedder: Option<&'a dyn Embedder>,
    /// Powers full-autonomy adjudication.
    pub llm: Option<&'a dyn ChatLlm>,
    pub autonomy: Autonomy,
    pub source_kind: SourceKind,
}

/// Write one extracted fact through the write-path v2 pipeline.
/// Test seam: make persistence of matching facts fail.
///
/// Set to a substring; any fact whose subject or predicate contains it fails
/// to persist. Fact persistence has no other way to fail on demand, and the
/// "extraction marker recorded even though the facts were never written" bug
/// class is exactly what has to stay covered — a memory marked done with no
/// facts in it is invisible to `facts backfill` forever. Same shape as
/// `ENGRAM_FACT_LLM_STUB`.
pub const WRITE_FAIL_ENV: &str = "ENGRAM_FACT_WRITE_FAIL";

pub async fn write_fact(
    ctx: &AppContext,
    w: &FactWriteCtx<'_>,
    ef: &ExtractedFact,
    outcome: &mut FactWriteOutcome,
) -> Result<(), CliError> {
    if let Ok(needle) = std::env::var(WRITE_FAIL_ENV) {
        if !needle.trim().is_empty()
            && (ef.subject.contains(needle.trim()) || ef.predicate.contains(needle.trim()))
        {
            return Err(CliError::Storage(engram_storage::StorageError::Migration(
                format!("{WRITE_FAIL_ENV} injected a persistence failure for {ef:?}"),
            )));
        }
    }
    let (memory_id, diary, kb) = (w.memory_id, w.diary, w.kb);
    let (embedder, llm, autonomy, source_kind) = (w.embedder, w.llm, w.autonomy, w.source_kind);
    let predicate = canonical_predicate(&ef.predicate);
    let (subject, subject_norm) = canonicalize_subject(ctx, diary, kb, &ef.subject);
    let object_norm = normalize(&ef.object);
    if subject_norm.is_empty() || predicate.is_empty() || object_norm.is_empty() {
        return Ok(());
    }

    let policy = slot_policy(&predicate);
    let fact_type = match (policy.is_some(), ef.fact_type.as_deref()) {
        (true, _) => FactType::ProfileSlot,
        (false, Some(t)) => FactType::parse(t),
        (false, None) => FactType::Assertion,
    };
    let valid_from = resolve_valid_from(ef, &predicate, w.event_time);

    let new_fact = Fact {
        id: Uuid::new_v4(),
        source_memory_id: memory_id,
        subject: subject.clone(),
        subject_norm: subject_norm.clone(),
        predicate: predicate.clone(),
        object: ef.object.clone(),
        object_norm: object_norm.clone(),
        confidence: ef.confidence,
        created_at: Utc::now(),
        superseded_by: None,
        superseded_at: None,
        diary: diary.to_string(),
        fact_type,
        valid_from,
        valid_until: None,
        resolution: None,
        source_kind,
        confirmations: 0,
    };

    // ---- Exact path: same (subject_norm, predicate) ----
    let exact = ctx
        .store
        .get_active_facts_in_kb(&subject_norm, &predicate, diary, kb)?;
    if let Some(same) = exact.iter().find(|f| f.object_norm == object_norm) {
        ctx.store.increment_fact_confirmations(same.id)?;
        outcome.deduped += 1;
        return Ok(());
    }
    if !exact.is_empty() {
        // Same question, different answer.
        match (policy, autonomy) {
            (Some(SlotPolicy::AwaitConfirmation), _) | (_, Autonomy::Strict) => {
                insert_with_embedding(ctx, &new_fact, embedder, kb, diary).await?;
                for old in &exact {
                    queue_conflict(ctx, &new_fact, old, diary, kb, "slot", None, outcome)?;
                }
                outcome.added += 1;
            }
            (Some(SlotPolicy::LastWriterWins), _) => {
                insert_with_embedding(ctx, &new_fact, embedder, kb, diary).await?;
                for old in &exact {
                    ctx.store.supersede_fact_with(
                        old.id,
                        new_fact.id,
                        "slot_lww",
                        new_fact.valid_from,
                    )?;
                    outcome
                        .conflicts
                        .push(conflict_payload(old, &new_fact, "slot_lww"));
                }
                outcome.added += 1;
            }
            (None, _) => {
                // Non-slot exact conflict: preserve the long-standing
                // supersede-on-exact-match behavior.
                insert_with_embedding(ctx, &new_fact, embedder, kb, diary).await?;
                for old in &exact {
                    ctx.store.supersede_fact_with(
                        old.id,
                        new_fact.id,
                        "exact_match",
                        new_fact.valid_from,
                    )?;
                    outcome
                        .conflicts
                        .push(conflict_payload(old, &new_fact, "exact_match"));
                }
                outcome.added += 1;
            }
        }
        return Ok(());
    }

    // ---- Semantic path: novelty gate over all active facts ----
    let Some(embedder) = embedder else {
        // No embedder (offline / no key): exact path already ran; insert.
        ctx.store.insert_fact(&new_fact)?;
        outcome.added += 1;
        return Ok(());
    };
    let text = fact_text(&subject, &predicate, &ef.object);
    let embedding = embedder
        .embed_batch(&[text.as_str()], TaskMode::RetrievalDocument)
        .await?
        .into_iter()
        .next()
        .unwrap_or_default();
    let existing = ctx
        .store
        .active_fact_embeddings(diary, kb, &embedder.model())?;
    let (dup_t, novel_t) = gate_thresholds();
    match novelty_gate(&embedding, &existing, dup_t, novel_t) {
        GateDecision::Duplicate { fact_id, .. } => {
            ctx.store.increment_fact_confirmations(fact_id)?;
            outcome.deduped += 1;
        }
        GateDecision::Novel => {
            ctx.store.insert_fact(&new_fact)?;
            ctx.store
                .set_fact_embedding(new_fact.id, &embedding, &embedder.model())?;
            outcome.added += 1;
        }
        GateDecision::Uncertain { candidates } => {
            let cand_facts: Vec<(Fact, f32)> = candidates
                .iter()
                .filter_map(|(id, s)| ctx.store.get_fact(*id).ok().flatten().map(|f| (f, *s)))
                .collect();
            match (autonomy, llm) {
                (Autonomy::Full, Some(llm)) => {
                    let rendered: Vec<String> = cand_facts
                        .iter()
                        .map(|(f, _)| {
                            format!("{} | {} {} {}", f.id, f.subject, f.predicate, f.object)
                        })
                        .collect();
                    let action = adjudicate(llm, &text, &rendered)
                        .await
                        .unwrap_or(AdjudicationAction::Coexist);
                    match action {
                        AdjudicationAction::RejectNew => {
                            outcome.deduped += 1;
                            return Ok(());
                        }
                        AdjudicationAction::Supersede(ids) => {
                            ctx.store.insert_fact(&new_fact)?;
                            ctx.store.set_fact_embedding(
                                new_fact.id,
                                &embedding,
                                &embedder.model(),
                            )?;
                            for (f, _) in &cand_facts {
                                if ids.iter().any(|i| i == &f.id.to_string()) {
                                    ctx.store.supersede_fact_with(
                                        f.id,
                                        new_fact.id,
                                        "semantic_llm",
                                        new_fact.valid_from,
                                    )?;
                                    outcome.conflicts.push(conflict_payload(
                                        f,
                                        &new_fact,
                                        "semantic_llm",
                                    ));
                                }
                            }
                            outcome.added += 1;
                        }
                        AdjudicationAction::Coexist => {
                            ctx.store.insert_fact(&new_fact)?;
                            ctx.store.set_fact_embedding(
                                new_fact.id,
                                &embedding,
                                &embedder.model(),
                            )?;
                            outcome.added += 1;
                        }
                    }
                }
                _ => {
                    // Assisted/strict (or full without an LLM): store the new
                    // fact as contested and hand the decision to the caller.
                    ctx.store.insert_fact(&new_fact)?;
                    ctx.store
                        .set_fact_embedding(new_fact.id, &embedding, &embedder.model())?;
                    for (f, score) in &cand_facts {
                        queue_conflict(
                            ctx,
                            &new_fact,
                            f,
                            diary,
                            kb,
                            "semantic_cosine",
                            Some(*score),
                            outcome,
                        )?;
                    }
                    outcome.added += 1;
                }
            }
        }
    }
    Ok(())
}

async fn insert_with_embedding(
    ctx: &AppContext,
    fact: &Fact,
    embedder: Option<&dyn Embedder>,
    _kb: &str,
    _diary: &str,
) -> Result<(), CliError> {
    ctx.store.insert_fact(fact)?;
    if let Some(e) = embedder {
        let text = fact_text(&fact.subject, &fact.predicate, &fact.object);
        if let Ok(mut v) = e
            .embed_batch(&[text.as_str()], TaskMode::RetrievalDocument)
            .await
        {
            if let Some(emb) = v.pop() {
                ctx.store.set_fact_embedding(fact.id, &emb, &e.model())?;
            }
        }
    }
    Ok(())
}

fn conflict_payload(old: &Fact, new: &Fact, resolution: &str) -> serde_json::Value {
    json!({
        "subject": old.subject,
        "predicate": old.predicate,
        "old_object": old.object,
        "new_object": new.object,
        "old_fact_id": old.id.to_string(),
        "new_fact_id": new.id.to_string(),
        "old_created_at": old.created_at.to_rfc3339(),
        "resolution": resolution,
    })
}

#[allow(clippy::too_many_arguments)]
fn queue_conflict(
    ctx: &AppContext,
    new_fact: &Fact,
    old_fact: &Fact,
    diary: &str,
    kb: &str,
    detector: &str,
    score: Option<f32>,
    outcome: &mut FactWriteOutcome,
) -> Result<(), CliError> {
    let qid = Uuid::new_v4();
    let suggested = if detector == "slot" {
        "supersede-old"
    } else {
        "keep-both"
    };
    if ctx.store.insert_fact_conflict(
        qid,
        new_fact.id,
        old_fact.id,
        diary,
        kb,
        detector,
        score,
        suggested,
    )? {
        outcome.queued.push(json!({
            "conflict_id": qid.to_string(),
            "detector": detector,
            "score": score,
            "new": fact_summary(new_fact),
            "old": fact_summary(old_fact),
            "suggested_action": suggested,
            "status": "contested",
            "resolve_command": format!(
                "engram facts resolve {qid} --action supersede-old|keep-both|reject-new"
            ),
            "note": "both values stay active until resolved; recall will report this subject as contested",
        }));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn autonomy_parses() {
        assert_eq!(Autonomy::parse("full").unwrap(), Autonomy::Full);
        assert_eq!(Autonomy::parse("assisted").unwrap(), Autonomy::Assisted);
        assert_eq!(Autonomy::parse("strict").unwrap(), Autonomy::Strict);
        assert!(Autonomy::parse("yolo").is_err());
    }

    fn extracted(predicate: &str, object: &str, valid_from: Option<&str>) -> ExtractedFact {
        ExtractedFact {
            subject: "Ana".into(),
            predicate: predicate.into(),
            object: object.into(),
            confidence: 1.0,
            fact_type: None,
            valid_from: valid_from.map(|s| s.to_string()),
        }
    }

    fn moment(s: &str) -> DateTime<Utc> {
        parse_moment(s).unwrap()
    }

    #[test]
    fn explicit_valid_from_wins() {
        let ef = extracted("lives_in", "Lisbon", Some("2026-03-01"));
        let got = resolve_valid_from(&ef, "lives_in", Some(moment("2020-01-01"))).unwrap();
        assert_eq!(got, moment("2026-03-01"));
    }

    #[test]
    fn date_valued_object_becomes_the_event_time() {
        let ef = extracted("born_on", "1990-04-12", None);
        let got = resolve_valid_from(&ef, "born_on", None).unwrap();
        assert_eq!(got, moment("1990-04-12"));
    }

    #[test]
    fn non_date_predicates_ignore_a_date_shaped_object() {
        let ef = extracted("lives_in", "1990-04-12", None);
        let event = moment("2024-06-01");
        assert_eq!(
            resolve_valid_from(&ef, "lives_in", Some(event)).unwrap(),
            event
        );
    }

    #[test]
    fn facts_inherit_the_memory_event_time() {
        let ef = extracted("married_to", "Bo", None);
        let event = moment("2019-08-17T14:00:00Z");
        assert_eq!(
            resolve_valid_from(&ef, "married_to", Some(event)).unwrap(),
            event
        );
    }

    #[test]
    fn undated_facts_fall_back_to_now() {
        let ef = extracted("prefers", "tea", Some("next spring"));
        let got = resolve_valid_from(&ef, "prefers", None).unwrap();
        assert!((Utc::now() - got).num_seconds().abs() < 5);
    }

    #[test]
    fn token_sort_normalizes_name_order() {
        assert_eq!(token_sort("smith, robert"), token_sort("robert smith"));
        assert_ne!(token_sort("robert smith"), token_sort("robert smythe"));
    }
}

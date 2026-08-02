//! Write-path v3: the APPLIER. It resolves an observation, loads the
//! intervals that could be affected, asks `engram_graph::semantics` what the
//! observation means, and performs exactly what came back.
//!
//! Per extracted fact:
//! 1. canonicalize the relation (registry synonyms + cessation forms) and the
//!    subject (alias/fuzzy cascade against known subjects), then normalize;
//! 2. resolve the INTERVAL the assertion covers — `valid_from`, and now
//!    `valid_until`, with the same discipline: a date nobody can read is
//!    reported and dropped, never silently set to now;
//! 3. load every fact on the same (subject, relation), open or closed — the
//!    only rows an interval decision can involve;
//! 4. `plan_write` decides, purely. Prior caller decisions are replayed before
//!    any question is re-asked;
//! 5. apply: insert, confirm, close, queue — in that order, inside the
//!    caller's transaction.
//!
//! WHY THE SPLIT. The classifier used to live here, and the arity it used was
//! derived from "does this predicate have a slot policy" — so
//! `takes_medication` was a single-valued slot and five concurrent
//! medications produced ten pairwise "conflicts", each suggesting the previous
//! drug be superseded. A guarantee that can only be tested by writing five
//! facts into SQLite and counting queue rows is a guarantee nobody checks. The
//! plan is a value: `plan_write` for the fifth medication yields an empty
//! `queue` and an empty `closes`, and that is an assertion over fixtures.
//!
//! The caller is a frontier agent with conversation context this store cannot
//! see — `assisted` mode hands it the adjudication instead of paying for a
//! worse-informed LLM call here. Under `full` the LLM may adjudicate, but it
//! is a `Heuristic` authority: it can no longer close a safety interval.

use crate::context::AppContext;
use crate::error::CliError;
use chrono::{DateTime, Utc};
use engram_core::interval::{may_close, CloseAuthority, CloseReason, CloseVerdict, Interval};
use engram_core::relations::{canonical_relation, cessation_of, member_identity, relation_spec};
use engram_core::temporal::parse_moment;
use engram_core::types::{Fact, FactType, SourceKind};
use engram_embed::{Embedder, TaskMode};
use engram_graph::facts::{
    adjudicate, canonical_predicate, date_valued_predicate, normalize, AdjudicationAction,
    ExtractedFact,
};
use engram_graph::gate::{novelty_gate, GateDecision};
use engram_graph::semantics::{
    classify_mode_for, compile_resolution, plan_write, DecisionLedger, Observation,
    ObservationState, PlanNote, PlannedClose, PlannedConflict, SemanticKey, UnknownAutonomy,
    WritePlan,
};
use engram_llm::ChatLlm;
use serde_json::json;
use std::collections::HashMap;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// The classifier moved to `engram_graph::semantics` so `facts triage`,
// `engram state` and the daemon can read the same rules without depending on
// the CLI crate. These re-exports keep one import path for the call sites that
// already had one; `LegacyArity` deliberately keeps its LEGACY NAME (R3) — an
// `as Arity` alias would be a public trap with no in-repo consumer.
// ---------------------------------------------------------------------------
pub use engram_core::relations::{is_safety_slot, predicate_arity, LegacyArity};
pub use engram_graph::semantics::{
    allowed_actions, classify_pair, classify_views, safety_gate, suggested_action, Autonomy,
    ClassifyMode, ConflictClass, Verdict, MULTI_VALUED_PREDICATES,
};

/// The autonomy parse error, carried across the crate boundary as a type so
/// `engram-graph` never has to know what a CLI error is.
impl From<UnknownAutonomy> for CliError {
    fn from(e: UnknownAutonomy) -> CliError {
        CliError::BadInput(e.to_string())
    }
}

/// The memory a batch of facts came from, as far as time is concerned.
///
/// Bundled rather than passed as two loose fields because they are only ever
/// meaningful together: `event_time` when it exists, otherwise `created_at` —
/// and a caller that reads one without the other reintroduces the fabricated
/// date this type exists to prevent.
#[derive(Debug, Clone, Copy)]
pub struct MemoryTime {
    /// `--event-time`: when the content actually happened.
    pub event_time: Option<DateTime<Utc>>,
    /// When the store recorded the memory. Always known.
    pub created_at: DateTime<Utc>,
}

impl MemoryTime {
    /// The date a fact inherits when it has none of its own, plus the name of
    /// that source for the warning payload.
    fn anchor(&self) -> (DateTime<Utc>, &'static str) {
        match self.event_time {
            Some(t) => (t, "memory_event_time"),
            None => (self.created_at, "memory_created_at"),
        }
    }
}

/// A date the extractor (or a `--facts-json` caller) supplied that could not be
/// read, and what was used in its place.
///
/// Reported, never swallowed: a fact quietly dated "now" because nobody could
/// parse "last March" is not a missing date, it is the NEWEST date in the
/// store, and it wins recency supersession against the older true fact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DateWarning {
    pub field: &'static str,
    /// The unreadable value, echoed back so the caller can fix the source.
    pub value: String,
    pub used: DateTime<Utc>,
    /// Where `used` came from: memory_event_time | memory_created_at |
    /// date_valued_object.
    pub fallback: &'static str,
}

impl DateWarning {
    /// The envelope payload: what was unreadable, what was used instead, and
    /// the exact spelling that would have worked.
    pub fn to_json(&self, subject: &str, predicate: &str) -> serde_json::Value {
        json!({
            "subject": subject,
            "predicate": predicate,
            "field": self.field,
            "unreadable_value": self.value,
            "used_instead": self.used.to_rfc3339(),
            "fallback": self.fallback,
            "hint": "emit valid_from as YYYY-MM-DD or RFC3339 (2026-03-01T09:30:00Z), or omit it — \
                     a date the store cannot read must never become today's date",
        })
    }
}

/// Echo an unreadable date back to the caller, bounded so a pathological
/// extractor cannot paste a whole memory into the envelope.
fn truncate_for_report(raw: &str) -> String {
    const MAX: usize = 64;
    if raw.chars().count() <= MAX {
        return raw.to_string();
    }
    raw.chars().take(MAX).chain(std::iter::once('…')).collect()
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
    /// Unreadable extractor/caller dates, with the date used instead.
    pub date_warnings: Vec<serde_json::Value>,
    /// Non-fatal reports from the planner: an unreadable `valid_until`, a
    /// cessation with nothing open to close, a prior decision replayed. Same
    /// JSON shape as `date_warnings`, so an agent parses one shape.
    pub plan_notes: Vec<serde_json::Value>,
}

impl FactWriteOutcome {
    /// Fold a COMMITTED batch's numbers into the run total.
    ///
    /// Only a committed batch may contribute. A batch that rolled back left
    /// nothing in the store, so reporting its counts — or its queued conflict
    /// ids, which reference rows that no longer exist — would describe a store
    /// that never happened.
    pub fn merge(&mut self, other: FactWriteOutcome) {
        self.added += other.added;
        self.deduped += other.deduped;
        self.conflicts.extend(other.conflicts);
        self.queued.extend(other.queued);
        self.date_warnings.extend(other.date_warnings);
        self.plan_notes.extend(other.plan_notes);
    }
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
/// 1. an explicit `valid_from` that PARSES ("2026-03-01", RFC3339);
/// 2. the object of a date-valued predicate — `born_on 1990-04-12` was true
///    in 1990, whatever today's date is;
/// 3. the parent memory's own date: its `--event-time`, or failing that when
///    the store recorded it.
///
/// An explicit date that does NOT parse ("last March") falls to 2/3 and is
/// REPORTED. It must never be read as `Utc::now()`: a fabricated now is not a
/// missing date, it is the newest date in the store, so it wins recency
/// supersession (`classify_pair`) against the older true fact and retires real
/// history over a string the parser could not read. The fix for unreadable
/// dates is the anchored extraction prompt, not a relative-date grammar here.
///
/// Returns the resolved time plus the warning, if any. Always `Some`: the
/// temporal model is never left NULL.
fn resolve_valid_from(
    ef: &ExtractedFact,
    predicate: &str,
    memory_time: MemoryTime,
) -> (Option<DateTime<Utc>>, Option<DateWarning>) {
    let supplied = ef
        .valid_from
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    if let Some(parsed) = supplied.and_then(parse_moment) {
        return (Some(parsed), None);
    }
    let (at, fallback) = date_valued_predicate(predicate)
        .then(|| parse_moment(&ef.object))
        .flatten()
        .map(|t| (t, "date_valued_object"))
        .unwrap_or_else(|| memory_time.anchor());
    let warning = supplied.map(|raw| DateWarning {
        field: "valid_from",
        value: truncate_for_report(raw),
        used: at,
        fallback,
    });
    (Some(at), warning)
}

/// When this fact STOPPED being true, if the text said so.
///
/// Same discipline as `resolve_valid_from` and for the same reason, mirrored:
/// an unreadable `valid_until` is DROPPED and reported, never guessed. A
/// guessed end date closes an interval that is still running — the exact
/// failure the safety gate exists to prevent, arriving through the date
/// parser instead of through the classifier. A `valid_until` that precedes
/// `valid_from` is also dropped: a negative-length interval is nonsense to
/// every consumer that reads it, and the clamp belongs at the close, not here.
fn resolve_valid_until(
    ef: &ExtractedFact,
    valid_from: Option<DateTime<Utc>>,
) -> (Option<DateTime<Utc>>, Option<PlanNote>) {
    let supplied = ef
        .valid_until
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let Some(raw) = supplied else {
        return (None, None);
    };
    let Some(parsed) = parse_moment(raw) else {
        return (
            None,
            Some(PlanNote::UnreadableValidUntil {
                raw: truncate_for_report(raw),
            }),
        );
    };
    if let Some(from) = valid_from {
        if parsed < from {
            return (
                None,
                Some(PlanNote::ValidUntilBeforeValidFrom {
                    raw: truncate_for_report(raw),
                }),
            );
        }
    }
    (Some(parsed), None)
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
    if let Ok(subjects) = ctx.store.distinct_fact_subjects(Some(diary), Some(kb)) {
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
    /// The memory's own dates. Facts with no readable date of their own
    /// inherit from here — never from a fresh clock read.
    pub memory_time: MemoryTime,
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

/// Test seam: queue EVERY conflict candidate, bypassing classification.
///
/// This is the pre-triage write path — the one that turned 2,953 imported
/// facts into 9,510 queued conflicts. `facts triage` exists to clean up
/// exactly the backlog this produces, so its tests need a way to build one
/// through the normal CLI. Same shape as `ENGRAM_FACT_WRITE_FAIL`; never set
/// in production use.
pub const QUEUE_ALL_ENV: &str = "ENGRAM_FACT_QUEUE_ALL";

fn queue_everything() -> bool {
    std::env::var(QUEUE_ALL_ENV)
        .map(|v| !v.trim().is_empty() && v != "0")
        .unwrap_or(false)
}

/// The vectors a fact batch will need, computed BEFORE it opens its
/// transaction.
///
/// The write path embeds one fact at a time and every one of those calls is a
/// network round-trip. Holding SQLite's write lock across them is the defect
/// the refinement plan (2.4) is removing from `compile`: a long write blocks
/// every concurrent `remember` for its whole duration, and 5s of busy_timeout
/// turns that into transient failures for someone else. So the batch pays for
/// its embeddings up front, in ONE request, with no transaction open.
///
/// Keyed on the exact text, so a miss is always safe: the canonical subject a
/// fact resolves to can change once an earlier fact in the same batch lands,
/// and that fact simply falls back to a live call.
#[derive(Default)]
pub struct FactEmbedCache {
    by_text: HashMap<String, Vec<f32>>,
}

impl FactEmbedCache {
    pub fn empty() -> Self {
        Self::default()
    }

    /// Embed every distinct text in one call. An arity mismatch yields an EMPTY
    /// cache rather than a guess: pairing a fact with another fact's vector is
    /// worse than paying for a second call.
    pub async fn warm(embedder: &dyn Embedder, texts: &[String]) -> Result<Self, CliError> {
        let mut unique: Vec<&str> = Vec::new();
        for text in texts {
            let t = text.as_str();
            if !t.trim().is_empty() && !unique.contains(&t) {
                unique.push(t);
            }
        }
        if unique.is_empty() {
            return Ok(Self::default());
        }
        let vectors = embedder
            .embed_batch(&unique, TaskMode::RetrievalDocument)
            .await?;
        if vectors.len() != unique.len() {
            return Ok(Self::default());
        }
        Ok(Self {
            by_text: unique
                .iter()
                .map(|t| (*t).to_string())
                .zip(vectors)
                .filter(|(_, v)| !v.is_empty())
                .collect(),
        })
    }

    fn get(&self, text: &str) -> Option<Vec<f32>> {
        self.by_text.get(text).cloned()
    }
}

/// The cached vector for `text`, or one live call.
///
/// An empty vector is an error, not a value: written to the fact row it becomes
/// an empty BLOB the novelty gate then skips forever — a fact that can never be
/// compared against anything.
async fn embed_fact_text(
    embedder: &dyn Embedder,
    cache: &FactEmbedCache,
    text: &str,
) -> Result<Vec<f32>, CliError> {
    if let Some(v) = cache.get(text) {
        return Ok(v);
    }
    let mut got = embedder
        .embed_batch(&[text], TaskMode::RetrievalDocument)
        .await?;
    match got.pop() {
        Some(v) if !v.is_empty() => Ok(v),
        _ => Err(CliError::Transient(
            "the embedder returned no vector for a fact; nothing from this batch was stored — \
             retry once the embedder is reachable"
                .into(),
        )),
    }
}

/// The texts this batch will embed, resolved against the store as it is NOW.
/// Both embed sites in `write_fact` build the same string, so one key per fact
/// covers the exact path and the semantic path alike.
fn batch_embed_texts(
    ctx: &AppContext,
    w: &FactWriteCtx<'_>,
    facts: &[ExtractedFact],
) -> Vec<String> {
    facts
        .iter()
        .map(|ef| {
            let predicate = canonical_predicate(&ef.predicate);
            let (subject, _) = canonicalize_subject(ctx, w.diary, w.kb, &ef.subject);
            fact_text(&subject, &predicate, &ef.object)
        })
        .collect()
}

/// The "already extracted" marker a batch earns by committing.
pub struct FactExtractionMarker<'a> {
    pub model: Option<&'a str>,
    /// `remember` | `backfill` — recorded for provenance.
    pub source: &'a str,
}

/// One memory's facts, plus the marker they earn.
pub struct FactBatch<'a> {
    pub facts: &'a [ExtractedFact],
    /// `None` when extraction did not run (`--facts-json`,
    /// `--no-extract-facts`, no key): the memory must stay in the backfill
    /// queue in that case, so it earns no marker even on success.
    pub marker: Option<FactExtractionMarker<'a>>,
}

/// Which fact aborted a batch, and why.
pub struct FactBatchError {
    /// 0-based index into `FactBatch::facts`; equals `total` when the marker
    /// itself (or the commit) was the write that failed.
    pub index: usize,
    pub total: usize,
    pub error: CliError,
}

impl FactBatchError {
    /// What failed, in words that survive the marker case: `index == total`
    /// is the marker/commit write, and rendering it as `fact 4 of 3` shipped
    /// impossible arithmetic to users and agents.
    pub fn what_failed(&self) -> String {
        Self::render_what_failed(self.index, self.total)
    }

    /// Same phrasing for callers that carry (index, total) without the error.
    pub fn render_what_failed(index: usize, total: usize) -> String {
        if index >= total {
            format!("the batch could not be committed after all {total} fact(s) were prepared")
        } else {
            format!("fact {} of {total} could not be stored", index + 1)
        }
    }
}

/// Persist one memory's whole fact batch — every fact AND the extraction
/// marker — as ONE unit of work.
///
/// THE FAILURE THIS REMOVES. `fact_extraction_log` is what makes a memory
/// invisible to every future `facts backfill`, and so is a single fact:
/// `PENDING_EXTRACTION_PREDICATE` counts a memory as processed once it has ANY
/// fact OR a marker. Writing facts one at a time therefore had a state where
/// fact A committed, fact B failed, no marker was written — and the memory left
/// the backfill queue anyway, on the strength of A. B was lost permanently and
/// silently. Managing that state means teaching the queue about half-processed
/// memories; removing it means the batch commits or it never happened.
///
/// Everything network-bound is paid for BEFORE the transaction opens (see
/// [`FactEmbedCache`]). One await can still occur inside it: `--autonomy full`
/// adjudicates an unsettled candidate with an LLM call. That is opt-in, bounded
/// by one call per contested fact, and the same defect class the refinement
/// plan's 2.4 removes from `compile`.
///
/// PRECONDITION: no transaction is open on this connection. Both call sites
/// (`remember`, `facts backfill`) are top-level; a nested call fails at
/// `BEGIN IMMEDIATE` and reports it rather than corrupting an outer unit.
/// `compile` is the exception that must NOT use this: it already holds its own
/// build-then-swap transaction, and calls [`write_fact`] directly.
pub async fn write_fact_batch(
    ctx: &AppContext,
    w: &FactWriteCtx<'_>,
    batch: &FactBatch<'_>,
    outcome: &mut FactWriteOutcome,
) -> Result<(), FactBatchError> {
    let total = batch.facts.len();
    let fail = |index: usize, error: CliError| FactBatchError {
        index,
        total,
        error,
    };

    let cache = match w.embedder {
        Some(embedder) => {
            let texts = batch_embed_texts(ctx, w, batch.facts);
            FactEmbedCache::warm(embedder, &texts)
                .await
                .map_err(|e| fail(0, e))?
        }
        None => FactEmbedCache::empty(),
    };

    ctx.store.begin_immediate().map_err(|e| fail(0, e.into()))?;
    let mut scratch = FactWriteOutcome::default();
    let mut failed: Option<(usize, CliError)> = None;
    for (i, ef) in batch.facts.iter().enumerate() {
        if let Err(e) = write_fact(ctx, w, ef, &cache, &mut scratch).await {
            tracing::warn!("fact {} of {total} failed to persist: {e}", i + 1);
            failed = Some((i, e));
            break;
        }
    }
    if failed.is_none() {
        if let Some(marker) = &batch.marker {
            if let Err(e) = ctx.store.record_fact_extraction(
                w.memory_id,
                marker.model,
                total as i64,
                marker.source,
            ) {
                failed = Some((total, CliError::from(e)));
            }
        }
    }
    match failed {
        None => match ctx.store.commit() {
            Ok(()) => {
                outcome.merge(scratch);
                Ok(())
            }
            Err(e) => {
                let _ = ctx.store.rollback();
                Err(fail(total, e.into()))
            }
        },
        Some((index, error)) => {
            let _ = ctx.store.rollback();
            Err(fail(index, error))
        }
    }
}

/// Everything the planner needs about one incoming assertion, resolved
/// against the store as it is now.
///
/// Returns `None` when the triple is not writable at all (an empty subject,
/// relation or object) — unchanged from write-path v2, and silent on purpose:
/// an extractor emitting a blank object is not a user-facing failure.
fn build_observation(
    ctx: &AppContext,
    w: &FactWriteCtx<'_>,
    ef: &ExtractedFact,
    outcome: &mut FactWriteOutcome,
) -> Option<Observation> {
    // A cessation predicate names the END of another relation's interval, not
    // a relation of its own: "stopped_taking" is a `takes_medication` fact
    // with `state: Ended`, and storing it under its own predicate is how a
    // stopped medication used to end up looking like a second one.
    let cessation = cessation_of(&ef.predicate);
    let relation = match cessation {
        Some(r) => r.to_string(),
        None => canonical_relation(&ef.predicate),
    };
    let state = if cessation.is_some() || ef.says_ended() {
        ObservationState::Ended
    } else {
        ObservationState::Ongoing
    };
    let (subject, subject_norm) = canonicalize_subject(ctx, w.diary, w.kb, &ef.subject);
    let object_norm = normalize(&ef.object);
    if subject_norm.is_empty() || relation.is_empty() || object_norm.is_empty() {
        return None;
    }
    let spec = relation_spec(&relation);
    // A profile slot is now defined by ARITY, not by "has a slot policy". The
    // safety sets stop being `profile_slot` because they are not slots — a
    // value change on a legacy field, recorded in the CHANGELOG, and nothing
    // in this repo branches on it.
    let fact_type = match (spec.is_single_valued(), ef.fact_type.as_deref()) {
        (true, _) => FactType::ProfileSlot,
        (false, Some(t)) => FactType::parse(t),
        (false, None) => FactType::Assertion,
    };
    let (valid_from, date_warning) = resolve_valid_from(ef, &relation, w.memory_time);
    if let Some(warning) = date_warning {
        // Recorded before any dedup/queue branch: the date was unreadable
        // whatever happens to the fact itself, and the caller is the only one
        // who can fix the source.
        outcome
            .date_warnings
            .push(warning.to_json(&subject, &relation));
    }
    let (valid_until, until_note) = resolve_valid_until(ef, valid_from);
    if let Some(note) = until_note {
        outcome.plan_notes.push(note.to_json(&subject, &relation));
    }
    let from = valid_from.unwrap_or(w.memory_time.created_at);
    Some(Observation {
        fact_id: Uuid::new_v4(),
        member: member_identity(&relation, &object_norm),
        subject,
        subject_norm,
        relation,
        spec,
        object: ef.object.clone(),
        object_norm,
        confidence: ef.confidence,
        source_memory_id: w.memory_id,
        source_kind: w.source_kind,
        fact_type,
        diary: w.diary.to_string(),
        kb: w.kb.to_string(),
        interval: Interval {
            from,
            until: valid_until,
        },
        state,
        autonomy: w.autonomy,
        queue_everything: queue_everything(),
    })
}

pub async fn write_fact(
    ctx: &AppContext,
    w: &FactWriteCtx<'_>,
    ef: &ExtractedFact,
    cache: &FactEmbedCache,
    outcome: &mut FactWriteOutcome,
) -> Result<(), CliError> {
    if let Ok(needle) = std::env::var(WRITE_FAIL_ENV) {
        let needle = needle.trim();
        if !needle.is_empty() && (ef.subject.contains(needle) || ef.predicate.contains(needle)) {
            // Never `{ef:?}`: the fact restates the memory it came from, and
            // this string is rendered into `fact_extraction_note`, which goes
            // to stdout AND stderr. Only the operator's own needle is echoed.
            // `Io`, not `Migration`: R11 classifies a migration failure as a
            // CONFIG error (exit 2, "fix your setup"), and this seam fabricates
            // a failed write, which is transient (exit 1, "retry the identical
            // command") — the advice the batch's own error message gives.
            return Err(CliError::Storage(engram_storage::StorageError::Io(
                std::io::Error::other(format!(
                    "{WRITE_FAIL_ENV} injected a persistence failure (needle '{needle}')"
                )),
            )));
        }
    }
    let Some(observation) = build_observation(ctx, w, ef, outcome) else {
        return Ok(());
    };
    let (diary, kb) = (w.diary, w.kb);

    // Every fact on this (subject, relation), OPEN OR CLOSED. Closed rows are
    // not optional: without them a restarted medication reads as a duplicate,
    // and a backdated assertion re-opens a window someone deliberately ended.
    // Only same-relation rows are considered, so the fifth medication compares
    // against zero facts rather than four.
    let siblings = ctx.store.facts_for_relation(
        Some(kb),
        Some(diary),
        &observation.subject_norm,
        &observation.relation,
        true,
    )?;

    let text = fact_text(
        &observation.subject,
        &observation.relation,
        &observation.object,
    );
    let mut embedding: Option<Vec<f32>> = None;
    let mut others: Vec<(Fact, f32)> = Vec::new();
    if let Some(embedder) = w.embedder {
        if siblings.is_empty() {
            // The novelty gate answers "is this new to the store at all", so
            // it only runs when nothing on this relation could answer it —
            // the same shape, and the same cost, as write-path v2.
            let emb = embed_fact_text(embedder, cache, &text).await?;
            let existing = ctx
                .store
                .active_fact_embeddings(diary, kb, &embedder.model())?;
            let (dup_t, novel_t) = gate_thresholds();
            match novelty_gate(&emb, &existing, dup_t, novel_t) {
                GateDecision::Duplicate { fact_id, .. } => {
                    ctx.store.increment_fact_confirmations(fact_id)?;
                    outcome.deduped += 1;
                    return Ok(());
                }
                GateDecision::Novel => {}
                GateDecision::Uncertain { candidates } => {
                    others = candidates
                        .iter()
                        .filter_map(|(id, s)| {
                            ctx.store.get_fact(*id).ok().flatten().map(|f| (f, *s))
                        })
                        .collect();
                }
            }
            embedding = Some(emb);
        } else {
            // The fact IS stored either way; a failed embed only costs the
            // novelty gate one vector. With the pre-warmed cache this makes no
            // call at all in the common path.
            embedding = embed_fact_text(embedder, cache, &text).await.ok();
        }
    }

    // The only thing in the planner that touches a store, and it propagates:
    // a failed decision lookup read as "nobody has decided this" re-asks a
    // settled safety question and can re-close an interval the caller
    // reinstated.
    let prior = |key: &SemanticKey| ctx.store.binding_decision(kb, key);
    let mut plan = plan_write(
        &observation,
        &siblings,
        &others,
        classify_mode_for(w.source_kind),
        &prior,
    )?;

    if w.autonomy == Autonomy::Full && !plan.queue.is_empty() {
        if let Some(llm) = w.llm {
            adjudicate_plan(llm, &text, &observation, &siblings, &others, &mut plan).await;
        }
    }

    apply_plan(
        ctx,
        w,
        &observation,
        &plan,
        &siblings,
        &others,
        embedding.as_deref(),
        outcome,
    )
}

/// `--autonomy full`: let this system's LLM settle what the classifier queued.
///
/// The LLM is a `Heuristic` authority and nothing more. Its `supersede` can
/// close a routine interval and CANNOT close a safety one — `may_close`
/// refuses, the pair stays queued, and the caller (who has the conversation)
/// decides. That is the one behaviour change `--autonomy full` sees: more
/// queued pairs on medical content, fewer everywhere else.
///
/// A failed adjudication is not an error: the pairs stay queued, which is what
/// `assisted` would have done anyway.
async fn adjudicate_plan(
    llm: &dyn ChatLlm,
    text: &str,
    observation: &Observation,
    siblings: &[Fact],
    others: &[(Fact, f32)],
    plan: &mut WritePlan,
) {
    let by_id: HashMap<Uuid, &Fact> = siblings
        .iter()
        .chain(others.iter().map(|(f, _)| f))
        .map(|f| (f.id, f))
        .collect();
    let rendered: Vec<String> = plan
        .queue
        .iter()
        .filter_map(|q| by_id.get(&q.old_fact_id))
        .map(|f| format!("{} | {} {} {}", f.id, f.subject, f.predicate, f.object))
        .collect();
    if rendered.is_empty() {
        return;
    }
    let action = adjudicate(llm, text, &rendered)
        .await
        .unwrap_or(AdjudicationAction::Coexist);
    match action {
        AdjudicationAction::RejectNew => {
            plan.insert = None;
            plan.queue.clear();
            plan.absorbed = Some("semantic_llm");
        }
        AdjudicationAction::Coexist => plan.queue.clear(),
        AdjudicationAction::Supersede(ids) => {
            let authority = CloseAuthority::Heuristic {
                rule: "semantic_llm",
            };
            let mut kept: Vec<PlannedConflict> = Vec::new();
            for q in std::mem::take(&mut plan.queue) {
                let named = ids.iter().any(|i| i == &q.old_fact_id.to_string());
                let allowed = by_id
                    .get(&q.old_fact_id)
                    .map(|f| relation_spec(&canonical_predicate(&f.predicate)))
                    .map(|spec| may_close(&spec, &authority) == CloseVerdict::Allow)
                    .unwrap_or(false);
                match (named, allowed) {
                    (true, true) => plan.closes.push(PlannedClose {
                        fact_id: q.old_fact_id,
                        at: observation.interval.from,
                        reason: CloseReason::Superseded,
                        authority: authority.clone(),
                        closed_by: Some(observation.fact_id),
                        resolution_tag: "semantic_llm",
                    }),
                    // Named but refused: an LLM is not evidence that a
                    // medication stopped. The question survives.
                    (true, false) => kept.push(q),
                    (false, _) => {}
                }
            }
            plan.queue = kept;
        }
    }
}

/// Carry out a plan. Nothing here decides anything: every branch is a
/// `WritePlan` field, in the order the plan promises (insert, confirm, close,
/// queue), inside the caller's transaction.
#[allow(clippy::too_many_arguments)]
fn apply_plan(
    ctx: &AppContext,
    w: &FactWriteCtx<'_>,
    observation: &Observation,
    plan: &WritePlan,
    siblings: &[Fact],
    others: &[(Fact, f32)],
    embedding: Option<&[f32]>,
    outcome: &mut FactWriteOutcome,
) -> Result<(), CliError> {
    let inserted = plan
        .insert
        .as_ref()
        .map(|_| observation.to_fact(Utc::now()));
    if let (Some(fact), Some(planned)) = (inserted.as_ref(), plan.insert.as_ref()) {
        ctx.store.insert_fact(fact)?;
        if let (Some(emb), Some(embedder)) = (embedding, w.embedder) {
            ctx.store
                .set_fact_embedding(fact.id, emb, &embedder.model())?;
        }
        outcome.added += 1;
        // A cessation with nothing open to close is born and then closed, so
        // the journal carries a reason instead of a mute pre-dead row.
        if let Some((reason, authority)) = &planned.close {
            let at = planned.interval.until.unwrap_or(planned.interval.from);
            ctx.store.close_fact_interval(
                fact.id,
                at,
                reason.as_str(),
                authority.as_str(),
                None,
                Some("explicit_end"),
            )?;
        }
    } else if plan.absorbed.is_some() {
        outcome.deduped += 1;
    }

    for id in &plan.confirm {
        ctx.store.increment_fact_confirmations(*id)?;
    }

    let by_id: HashMap<Uuid, &Fact> = siblings
        .iter()
        .chain(others.iter().map(|(f, _)| f))
        .chain(inserted.iter())
        .map(|f| (f.id, f))
        .collect();

    for close in &plan.closes {
        let tag = resolution_tag(w.source_kind, close.resolution_tag);
        let closed = ctx.store.close_fact_interval(
            close.fact_id,
            close.at,
            close.reason.as_str(),
            close.authority.as_str(),
            close.closed_by,
            Some(tag),
        )?;
        if !closed {
            continue;
        }
        if let Some(loser) = by_id.get(&close.fact_id) {
            let winner = close.closed_by.and_then(|id| by_id.get(&id)).copied();
            outcome
                .conflicts
                .push(conflict_payload(loser, winner.unwrap_or(loser), tag));
        }
    }

    // A replay is only real once its effect is: one `decision.replayed` per
    // applied decision, in the same transaction as the write it caused.
    for replayed in &plan.replays {
        ctx.store.record_replay(
            w.kb,
            w.diary,
            replayed.decision_id,
            replayed.new_fact_id,
            Some(replayed.old_fact_id),
            replayed.verdict.as_str(),
            &format!("cli:{}", w.diary),
        )?;
    }

    for queued in &plan.queue {
        queue_conflict(ctx, w, observation, queued, &by_id, outcome)?;
    }

    for note in &plan.notes {
        outcome
            .plan_notes
            .push(note.to_json(&observation.subject, &observation.relation));
    }
    Ok(())
}

/// The audit tag that lands in `facts.resolution`: it says WHO retired a value
/// months later, so a compiled supersession does not borrow the write path's
/// namespace.
fn resolution_tag(source_kind: SourceKind, rule: &'static str) -> &'static str {
    match source_kind {
        SourceKind::CompileLlm => compile_resolution(rule),
        _ => rule,
    }
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

/// Open one question, carrying the semantic key that lets it be answered once.
///
/// Three things changed here. The suggestion comes from `suggested_action`
/// rather than from `detector == "slot"` — that hard-coded string advised
/// `supersede-old` on every slot pair, allergies included. The row carries
/// `decision_key`, `member_key` and `registry_version`, so the answer survives
/// a re-derivation and a registry change can re-key instead of re-litigate.
/// And the command is built by `respond.rs`: ONE runnable command, no pipes.
fn queue_conflict(
    ctx: &AppContext,
    w: &FactWriteCtx<'_>,
    observation: &Observation,
    planned: &PlannedConflict,
    by_id: &HashMap<Uuid, &Fact>,
    outcome: &mut FactWriteOutcome,
) -> Result<(), CliError> {
    let Some(old_fact) = by_id.get(&planned.old_fact_id).copied() else {
        return Ok(());
    };
    let new_fact = by_id
        .get(&planned.new_fact_id)
        .copied()
        .cloned()
        .unwrap_or_else(|| observation.to_fact(Utc::now()));
    let qid = Uuid::new_v4();
    let key = planned.key.encode();
    let insert_outcome = ctx.store.insert_fact_conflict(
        qid,
        planned.new_fact_id,
        planned.old_fact_id,
        w.diary,
        w.kb,
        planned.detector,
        planned.score,
        planned.suggested,
        &key,
        &planned.member_key,
        planned.registry_version,
    )?;
    // A key collision is REPORTED, not swallowed, and the two collisions are
    // told apart. `Ok(false)` used to mean "nothing was inserted" for two
    // unrelated reasons, and the recovery built on it could name a conflict id
    // for a different pair — or nothing at all.
    let (conflict_id, deduplicated) = match insert_outcome {
        engram_storage::InsertOutcome::Inserted => (qid, false),
        // A DIFFERENT pair already asks this exact question. Storage matched
        // the semantic key, so `deduplicated` is honest here and only here.
        engram_storage::InsertOutcome::DuplicateKey { existing } => (existing, true),
        // The same pair is already queued. Nothing is inferred.
        engram_storage::InsertOutcome::DuplicatePair { existing } => (existing, false),
    };
    let resolve = crate::respond::resolve_conflict(conflict_id, planned.allowed);
    let mut obj = serde_json::Map::new();
    obj.insert("conflict_id".into(), json!(conflict_id.to_string()));
    obj.insert("detector".into(), json!(planned.detector));
    obj.insert("score".into(), json!(planned.score));
    obj.insert("rule".into(), json!(planned.verdict.rule));
    obj.insert("new".into(), fact_summary(&new_fact));
    obj.insert("old".into(), fact_summary(old_fact));
    obj.insert("suggested_action".into(), json!(planned.suggested));
    obj.insert("status".into(), json!("contested"));
    obj.insert("decision_key".into(), json!(key));
    if deduplicated {
        obj.insert("deduplicated".into(), json!(true));
    }
    crate::respond::attach(&mut obj, &resolve);
    obj.insert(
        "note".into(),
        json!(
            "both values stay active until resolved; recall will report this subject as contested"
        ),
    );
    outcome.queued.push(serde_json::Value::Object(obj));
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
            valid_from: valid_from.map(|s| s.to_string()),
            ..Default::default()
        }
    }

    fn ending(predicate: &str, object: &str, until: Option<&str>) -> ExtractedFact {
        ExtractedFact {
            valid_until: until.map(|s| s.to_string()),
            state: Some("ended".into()),
            ..extracted(predicate, object, None)
        }
    }

    fn moment(s: &str) -> DateTime<Utc> {
        parse_moment(s).unwrap()
    }

    fn memory_time(event: Option<&str>) -> MemoryTime {
        MemoryTime {
            event_time: event.map(moment),
            created_at: moment("2026-07-01T09:00:00Z"),
        }
    }

    #[test]
    fn explicit_valid_from_wins() {
        let ef = extracted("lives_in", "Lisbon", Some("2026-03-01"));
        let (got, warning) = resolve_valid_from(&ef, "lives_in", memory_time(Some("2020-01-01")));
        assert_eq!(got.unwrap(), moment("2026-03-01"));
        assert!(warning.is_none(), "a readable date is not a warning");
    }

    #[test]
    fn date_valued_object_becomes_the_event_time() {
        let ef = extracted("born_on", "1990-04-12", None);
        let (got, warning) = resolve_valid_from(&ef, "born_on", memory_time(None));
        assert_eq!(got.unwrap(), moment("1990-04-12"));
        assert!(warning.is_none());
    }

    #[test]
    fn non_date_predicates_ignore_a_date_shaped_object() {
        let ef = extracted("lives_in", "1990-04-12", None);
        let (got, warning) = resolve_valid_from(&ef, "lives_in", memory_time(Some("2024-06-01")));
        assert_eq!(got.unwrap(), moment("2024-06-01"));
        assert!(warning.is_none());
    }

    #[test]
    fn facts_inherit_the_memory_event_time() {
        let ef = extracted("married_to", "Bo", None);
        let (got, warning) =
            resolve_valid_from(&ef, "married_to", memory_time(Some("2019-08-17T14:00:00Z")));
        assert_eq!(got.unwrap(), moment("2019-08-17T14:00:00Z"));
        assert!(warning.is_none());
    }

    #[test]
    fn an_unreadable_date_falls_back_to_the_memory_date_never_to_now() {
        let ef = extracted("prefers", "tea", Some("next spring"));
        let mt = memory_time(Some("2019-08-17T14:00:00Z"));
        let (got, warning) = resolve_valid_from(&ef, "prefers", mt);
        assert_eq!(
            got.unwrap(),
            moment("2019-08-17T14:00:00Z"),
            "a date the parser cannot read must not become the newest date in the store"
        );
        let warning = warning.expect("the caller has to learn the date was dropped");
        assert_eq!(warning.value, "next spring");
        assert_eq!(warning.fallback, "memory_event_time");
    }

    #[test]
    fn with_no_event_time_an_unreadable_date_falls_back_to_the_record_time() {
        let ef = extracted("prefers", "tea", Some("next spring"));
        let (got, warning) = resolve_valid_from(&ef, "prefers", memory_time(None));
        assert_eq!(got.unwrap(), moment("2026-07-01T09:00:00Z"));
        assert_eq!(warning.unwrap().fallback, "memory_created_at");
    }

    #[test]
    fn a_fact_with_no_date_at_all_inherits_the_memory_and_warns_about_nothing() {
        let ef = extracted("prefers", "tea", None);
        let (got, warning) = resolve_valid_from(&ef, "prefers", memory_time(None));
        assert_eq!(got.unwrap(), moment("2026-07-01T09:00:00Z"));
        assert!(warning.is_none(), "no date supplied is not a bad date");
    }

    #[test]
    fn a_blank_valid_from_is_an_absent_one() {
        let ef = extracted("prefers", "tea", Some("   "));
        let (got, warning) = resolve_valid_from(&ef, "prefers", memory_time(None));
        assert_eq!(got.unwrap(), moment("2026-07-01T09:00:00Z"));
        assert!(warning.is_none());
    }

    #[test]
    fn an_unreadable_date_never_reaches_the_envelope_unbounded() {
        let long = "x".repeat(500);
        let ef = extracted("prefers", "tea", Some(&long));
        let (_, warning) = resolve_valid_from(&ef, "prefers", memory_time(None));
        assert!(warning.unwrap().value.chars().count() <= 65);
    }

    #[test]
    fn token_sort_normalizes_name_order() {
        assert_eq!(token_sort("smith, robert"), token_sort("robert smith"));
        assert_ne!(token_sort("robert smith"), token_sort("robert smythe"));
    }

    // ----- valid_until: the other half of the interval -----

    #[test]
    fn a_readable_end_date_closes_the_interval() {
        let ef = ending("takes_medication", "metformin", Some("2026-06-01"));
        let (until, note) = resolve_valid_until(&ef, Some(moment("2026-01-01")));
        assert_eq!(until.unwrap(), moment("2026-06-01"));
        assert!(note.is_none());
        assert!(ef.says_ended());
    }

    #[test]
    fn an_unreadable_end_date_is_dropped_and_reported_never_guessed() {
        // A guessed end date closes an interval that is still running — the
        // safety failure, arriving through the date parser.
        let ef = ending("takes_medication", "metformin", Some("some time last year"));
        let (until, note) = resolve_valid_until(&ef, Some(moment("2026-01-01")));
        assert_eq!(until, None);
        assert!(matches!(note, Some(PlanNote::UnreadableValidUntil { .. })));
    }

    #[test]
    fn an_end_before_the_start_is_dropped_not_stored_backwards() {
        let ef = ending("lives_in", "Lisbon", Some("2019-01-01"));
        let (until, note) = resolve_valid_until(&ef, Some(moment("2026-01-01")));
        assert_eq!(until, None);
        assert!(matches!(
            note,
            Some(PlanNote::ValidUntilBeforeValidFrom { .. })
        ));
    }

    #[test]
    fn no_end_date_is_not_a_warning() {
        let ef = extracted("lives_in", "Lisbon", None);
        let (until, note) = resolve_valid_until(&ef, Some(moment("2026-01-01")));
        assert_eq!(until, None);
        assert!(note.is_none());
        assert!(!ef.says_ended(), "silence is not a cessation");
    }

    #[test]
    fn a_rolled_back_batch_contributes_nothing_to_the_run_total() {
        let mut total = FactWriteOutcome {
            added: 2,
            deduped: 1,
            ..Default::default()
        };
        let committed = FactWriteOutcome {
            added: 3,
            deduped: 0,
            ..Default::default()
        };
        total.merge(committed);
        assert_eq!(total.added, 5);
        assert_eq!(total.deduped, 1);
        // A batch that never committed is simply never merged — the numbers it
        // would have reported describe rows that do not exist.
    }

    #[test]
    fn a_compile_supersession_says_the_compiler_did_it() {
        assert_eq!(compile_resolution("recency_supersede"), "compile_recency");
        assert_eq!(compile_resolution("recency_stale"), "compile_stale");
        assert_eq!(compile_resolution("confidence_gap"), "compile_confidence");
        // Non-supersede rules keep the classifier's own name.
        assert_eq!(compile_resolution("duplicate"), "duplicate");
        assert_eq!(compile_resolution("safety_slot"), "safety_slot");
    }
}

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
//! Before any of that, a batch is GROUNDED against the memory it came from
//! (see [`GroundingJudge`]): an extracted safety fact the text does not
//! support never reaches the planner.
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
//! see — `assisted` mode hands it the adjudication. Under `full` a JEV judge
//! answers four calibrated questions per queued pair and a code rule
//! ([`conflict_rule`]) settles what they make clear. It is a `Heuristic`
//! authority: it never closes a safety interval, clears a safety question, or
//! discards a safety observation, and safety pairs are never sent to it.

use crate::context::AppContext;
use crate::error::CliError;
use crate::judge::{judge_mode, record_judge_usage, resolve_judge, JudgeMode, JudgeSite};
use chrono::{DateTime, Utc};
use engram_core::interval::{may_close, CloseAuthority, CloseReason, CloseVerdict, Interval};
use engram_core::relations::{canonical_relation, cessation_of, member_identity, relation_spec};
use engram_core::temporal::parse_moment;
use engram_core::types::{Fact, FactType, SourceKind};
use engram_embed::{Embedder, TaskMode};
use engram_graph::facts::{canonical_predicate, date_valued_predicate, normalize, ExtractedFact};
use engram_graph::gate::{novelty_gate, GateDecision};
use engram_graph::semantics::{
    classify_mode_for, compile_resolution, end_resolution_tag, plan_write, DecisionLedger,
    Observation, ObservationState, PlanNote, PlannedClose, PlannedConflict, SemanticKey,
    UnknownAutonomy, WritePlan, SEMANTIC_KEY_VERSION,
};
use engram_llm::judge::lexical::{negated_object, numbers_ok};
use engram_llm::judge::questions::{
    admits, fact_as_claim_with_state, grounding_request, pair_request, FactTriple, PairAnswers,
    REMEMBER_SAFETY_ADMIT_P,
};
use engram_llm::judge::{ask_batched, Judge, JudgeError};
use serde_json::json;
use std::collections::{BTreeMap, HashMap};
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
    /// The rows this batch actually INSERTED, in write order. They are the
    /// `derivation.ran` output list, which is what lets `admin rebuild`
    /// reconstruct the extraction marker instead of losing it [G6].
    pub fact_ids: Vec<Uuid>,
    /// Measurement: what the novelty gate did. `facts_deduped` folds a gate
    /// duplicate, a planner confirmation and a judge's `reject_new` into one
    /// number; these tell them apart.
    pub gate: GateCounters,
    /// Measurement: the questions handed to the caller, by the rule that
    /// queued them.
    pub queued_by_rule: BTreeMap<&'static str, usize>,
    /// Measurement: what the `--autonomy full` judge was asked and answered.
    pub adjudication: AdjudicationCounters,
    /// One entry per fact the grounding judge saw, plus every fact dropped
    /// by the negated-object rule (see [`GroundingJudge`]). A dropped fact is
    /// listed with its triple so the caller can re-supply it with
    /// `--facts-json` if it is true.
    pub grounding: Vec<serde_json::Value>,
    pub grounding_counts: GroundingCounters,
}

impl FactWriteOutcome {
    /// Fold a COMMITTED batch's numbers into the run total.
    ///
    /// Only a committed batch may contribute. A batch that rolled back left
    /// nothing in the store, so reporting its counts — or its queued conflict
    /// ids, which reference rows that no longer exist — would describe a store
    /// that never happened. That includes the judge calls it paid for.
    pub fn merge(&mut self, other: FactWriteOutcome) {
        self.added += other.added;
        self.deduped += other.deduped;
        self.conflicts.extend(other.conflicts);
        self.queued.extend(other.queued);
        self.date_warnings.extend(other.date_warnings);
        self.plan_notes.extend(other.plan_notes);
        self.fact_ids.extend(other.fact_ids);
        self.gate.merge(other.gate);
        for (rule, n) in other.queued_by_rule {
            *self.queued_by_rule.entry(rule).or_default() += n;
        }
        self.adjudication.merge(other.adjudication);
        self.grounding.extend(other.grounding);
        self.grounding_counts.merge(other.grounding_counts);
    }

    /// The measurement counters, for a command's `metadata`.
    pub fn counters(&self) -> [(&'static str, serde_json::Value); 4] {
        [
            ("gate", json!(self.gate)),
            ("queued_by_rule", json!(self.queued_by_rule)),
            ("adjudication", json!(self.adjudication)),
            ("grounding", json!(self.grounding_counts)),
        ]
    }
}

/// What grounding did, one count per fact unless named otherwise.
#[derive(Debug, Default, Clone, Copy, serde::Serialize)]
pub struct GroundingCounters {
    /// Facts the judge answered.
    pub judged: usize,
    pub dropped_by_judge: usize,
    pub dropped_negated_object: usize,
    /// Facts sent to the judge that came back unanswered (a failed call
    /// leaves every one of them unanswered). They are written as before.
    pub judge_failed: usize,
    /// Summed over the judge responses.
    pub input_tokens: u64,
    pub cost_usd: f64,
}

impl GroundingCounters {
    /// Facts grounding kept out of the store, by either rule.
    pub fn dropped(&self) -> usize {
        self.dropped_by_judge + self.dropped_negated_object
    }

    fn merge(&mut self, o: GroundingCounters) {
        self.judged += o.judged;
        self.dropped_by_judge += o.dropped_by_judge;
        self.dropped_negated_object += o.dropped_negated_object;
        self.judge_failed += o.judge_failed;
        self.input_tokens += o.input_tokens;
        self.cost_usd += o.cost_usd;
    }
}

/// What the novelty gate did, one count per fact that reached it.
#[derive(Debug, Default, Clone, Copy, serde::Serialize)]
pub struct GateCounters {
    pub ran: usize,
    /// Not run: the (subject, relation) already has facts to compare with.
    pub skipped_has_siblings: usize,
    pub duplicate: usize,
    pub novel: usize,
    pub uncertain: usize,
    /// Candidates the uncertain band handed to the planner.
    pub uncertain_candidates: usize,
}

impl GateCounters {
    fn merge(&mut self, o: GateCounters) {
        self.ran += o.ran;
        self.skipped_has_siblings += o.skipped_has_siblings;
        self.duplicate += o.duplicate;
        self.novel += o.novel;
        self.uncertain += o.uncertain;
        self.uncertain_candidates += o.uncertain_candidates;
    }
}

/// What the write-path judge was asked and answered.
#[derive(Debug, Default, Clone, Copy, serde::Serialize)]
pub struct AdjudicationCounters {
    /// One per observation whose queued pairs were sent to a judge.
    pub calls: usize,
    /// Calls that returned no verdict.
    pub failed: usize,
    pub supersede: usize,
    pub coexist: usize,
    pub reject_new: usize,
    /// Verdicts not applied: the pairs changed before the write.
    pub stale: usize,
    /// Observations whose pairs arose after judging (an earlier fact of the
    /// same batch landed): no judge saw them, and they stay queued.
    pub not_asked: usize,
    /// Pairs sent to a judge.
    pub pairs_in: usize,
    /// Intervals an applied verdict closed.
    pub pairs_closed: usize,
    /// Pairs still queued for the caller after an applied verdict.
    pub pairs_kept: usize,
}

impl AdjudicationCounters {
    fn merge(&mut self, o: AdjudicationCounters) {
        self.calls += o.calls;
        self.failed += o.failed;
        self.supersede += o.supersede;
        self.coexist += o.coexist;
        self.reject_new += o.reject_new;
        self.stale += o.stale;
        self.not_asked += o.not_asked;
        self.pairs_in += o.pairs_in;
        self.pairs_closed += o.pairs_closed;
        self.pairs_kept += o.pairs_kept;
    }
}

pub const DUP_THRESHOLD_ENV: &str = "ENGRAM_FACT_DUP_THRESHOLD";
pub const NOVEL_THRESHOLD_ENV: &str = "ENGRAM_FACT_NOVEL_THRESHOLD";

/// Thresholds for the novelty gate, env-overridable so they can be tuned on
/// the bench harness without a rebuild.
///
/// A value that does not parse, is not a cosine in [-1, 1], or leaves no
/// uncertain band (novel >= dup) is a `Config` error. It used to fall back
/// to the default in silence, so a tuning run measured the default while
/// its operator believed the override was live. Write surfaces call this
/// before they store anything, so the error lands before a half-write.
pub fn gate_thresholds() -> Result<(f32, f32), CliError> {
    parse_gate_thresholds(
        std::env::var(DUP_THRESHOLD_ENV).ok().as_deref(),
        std::env::var(NOVEL_THRESHOLD_ENV).ok().as_deref(),
    )
}

/// `gate_thresholds` over explicit values; blank reads as unset.
fn parse_gate_thresholds(dup: Option<&str>, novel: Option<&str>) -> Result<(f32, f32), CliError> {
    let read = |name: &str, raw: Option<&str>, default: f32| {
        let Some(raw) = raw.map(str::trim).filter(|r| !r.is_empty()) else {
            return Ok(default);
        };
        match raw.parse::<f32>() {
            Ok(v) if (-1.0..=1.0).contains(&v) => Ok(v),
            _ => Err(CliError::Config(format!(
                "{name}='{raw}' is not a cosine similarity in [-1, 1]; fix or unset it"
            ))),
        }
    };
    let dup = read(
        DUP_THRESHOLD_ENV,
        dup,
        engram_graph::gate::DEFAULT_DUP_THRESHOLD,
    )?;
    let novel = read(
        NOVEL_THRESHOLD_ENV,
        novel,
        engram_graph::gate::DEFAULT_NOVEL_THRESHOLD,
    )?;
    if novel >= dup {
        return Err(CliError::Config(format!(
            "{NOVEL_THRESHOLD_ENV} ({novel}) must be below {DUP_THRESHOLD_ENV} ({dup}): the \
             novelty gate needs an uncertain band between them; fix or unset them"
        )));
    }
    Ok((dup, novel))
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
    state: ObservationState,
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
    // The ordering check applies to an assertion that carries its own window.
    // A CESSATION has no window of its own — "she stopped taking it in June"
    // says nothing about when she started, so its `valid_from` is only the
    // date the store heard about it, and comparing the two would throw away
    // every end date older than the memory reporting it. The interval that
    // actually closes is clamped against ITS own start at close time.
    if state == ObservationState::Ongoing {
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

/// A stored fact on a safety relation (takes_medication, allergic_to, …).
fn is_safety_fact(f: &Fact) -> bool {
    relation_spec(&canonical_predicate(&f.predicate)).is_safety()
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
    /// The JEV judge `--autonomy full` asks about queued pairs. `None` under
    /// any other autonomy, or when no judge is reachable: the pairs queue.
    pub adjudicator: Option<&'a dyn Judge>,
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
    /// `content_hash` of the memory the extractor read. The derivation is
    /// keyed by (content, extractor), not by memory id, so an `edit` that
    /// restores earlier text finds the earlier run instead of paying for it
    /// twice.
    pub content_hash: &'a str,
}

/// One memory's facts, plus the marker they earn.
pub struct FactBatch<'a> {
    pub facts: &'a [ExtractedFact],
    /// `None` when extraction did not run (`--facts-json`,
    /// `--no-extract-facts`, no key): the memory must stay in the backfill
    /// queue in that case, so it earns no marker even on success.
    pub marker: Option<FactExtractionMarker<'a>>,
    /// The memory text the facts came from: the passage they are grounded
    /// against.
    pub source_text: &'a str,
    /// `None`: no judge is asked (grounding is off, or nothing can answer);
    /// only the negated-object rule runs.
    pub grounding: Option<&'a GroundingJudge>,
}

/// The grounding judge for `remember`, `memory add` and `facts backfill`: is
/// each fact supported by the memory it was taken from?
///
/// Engram's own extractor invents facts. On the remember eval (161 facts,
/// `benchmarks/jev-grounding-remember-20260925T1030.json`) 16% were
/// unsupported, 22% on health notes: "Sam allergic_to ibuprofen" from "avoid
/// ibuprofen with the kidney issue", "takes_medication paracetamol" from
/// "paracetamol is fine". They reached `state` unchecked. The rules, per
/// fact:
///
/// - Every mode, no judge needed: an EXTRACTED fact on a safety set
///   (takes_medication, allergic_to, diagnosed_with) whose object is a
///   negation ("no blood thinners") is dropped. The text does say it, so the
///   judge passes it; the shape is wrong, and `state` would list "no blood
///   thinners" as a medication.
/// - `on`: an extracted safety-set fact is written only if its numbers are
///   in the text and the judge admits it at [`REMEMBER_SAFETY_ADMIT_P`].
///   Other extracted facts are written and annotated: gating them drops true
///   preferences the judge reads literally.
/// - `shadow`: written exactly as `off`, annotated.
/// - A caller's fact (`--facts-json`) is never dropped, so it is never sent
///   to the judge either: that would carry the memory to a third party for
///   a verdict nothing uses. It is annotated `not_judged` / `caller_asserted`.
///   The caller has the conversation; the judge has one memory.
/// - No answer (a failed call, a dropped question) writes as before and is
///   counted as `judge_failed`: an outage must not delete facts.
///
/// One request per batch, sent before the write lock is taken.
pub struct GroundingJudge {
    mode: JudgeMode,
    judge: Box<dyn Judge>,
}

impl GroundingJudge {
    /// The grounding judge a command should use, or `None` when grounding is
    /// off or no judge is reachable (no key, `--offline`,
    /// `ENGRAM_BENCH_FORCE_STUB`). Resolve once per command, before anything
    /// is written: a bad setting is exit 2, not a half-write.
    pub fn resolve(offline: bool) -> Result<Option<Self>, CliError> {
        let mode = judge_mode(JudgeSite::Grounding)?;
        if mode == JudgeMode::Off {
            return Ok(None);
        }
        Ok(resolve_judge(offline)?.map(|judge| GroundingJudge { mode, judge }))
    }
}

/// Is a relation a safety SET? The only relations grounding may drop from:
/// a blood type of "O negative" is a value, not a negation, and the eval
/// measured the cut on these three.
fn is_safety_set(relation: &str) -> bool {
    let spec = relation_spec(relation);
    spec.is_safety() && spec.is_set_valued()
}

/// GATHER, before the embeddings and the adjudication calls: which of
/// `batch.facts` may be written. Reports into `outcome`.
async fn ground_batch(
    ctx: &AppContext,
    w: &FactWriteCtx<'_>,
    batch: &FactBatch<'_>,
    outcome: &mut FactWriteOutcome,
) -> Vec<bool> {
    let extracted = w.source_kind == SourceKind::AgentExtracted;
    // The claim is the fact as it will be stored: a cessation predicate
    // names its base relation, and an ended fact reads "no longer".
    let facts: Vec<(String, bool, bool)> = batch
        .facts
        .iter()
        .map(|ef| {
            let cessation = cessation_of(&ef.predicate);
            let relation = match cessation {
                Some(r) => r.to_string(),
                None => canonical_relation(&ef.predicate),
            };
            let ended = cessation.is_some() || ef.says_ended();
            let claim = fact_as_claim_with_state(&ef.subject, &relation, &ef.object, ended);
            let safety = is_safety_set(&relation);
            let negated = extracted && safety && negated_object(&ef.object);
            (claim, safety, negated)
        })
        .collect();
    let id = |i: usize| format!("f{i}");

    let judge = batch.grounding;
    // Only extracted facts can be dropped, so only they are asked about: a
    // caller's batch makes no request at all.
    let claims: Vec<(String, String)> = facts
        .iter()
        .enumerate()
        .filter(|(_, (_, _, negated))| extracted && !negated)
        .map(|(i, (claim, _, _))| (id(i), claim.clone()))
        .collect();
    let response = match judge {
        Some(g) if !claims.is_empty() => {
            let request = grounding_request(batch.source_text, &claims);
            match ask_batched(g.judge.as_ref(), &request).await {
                Ok(r) => {
                    record_judge_usage(
                        ctx,
                        JudgeSite::Grounding.operation(),
                        &r,
                        Some(w.kb),
                        Some(w.diary),
                    );
                    outcome.grounding_counts.input_tokens += r.input_tokens;
                    outcome.grounding_counts.cost_usd += r.cost_usd;
                    Some(r)
                }
                Err(e) => {
                    // Logged, never echoed: the facts are written as before.
                    tracing::warn!("grounding judge gave no answer; facts written unjudged: {e}");
                    None
                }
            }
        }
        _ => None,
    };

    let counts = &mut outcome.grounding_counts;
    let mut keep = Vec::with_capacity(facts.len());
    for (i, (ef, (claim, safety, negated))) in batch.facts.iter().zip(&facts).enumerate() {
        let p = response.as_ref().and_then(|r| r.noul(&id(i)).ok());
        let numbers = numbers_ok(claim, batch.source_text);
        let supported = p.map(|p| numbers && admits(p, REMEMBER_SAFETY_ADMIT_P));
        let unsupported_extracted_safety = extracted && *safety && supported == Some(false);
        let drop = *negated
            || (unsupported_extracted_safety && judge.is_some_and(|g| g.mode == JudgeMode::On));
        keep.push(!drop);
        let reason = if *negated {
            counts.dropped_negated_object += 1;
            "negated_object"
        } else if judge.is_none() {
            continue;
        } else if !extracted {
            "caller_asserted"
        } else if p.is_none() {
            counts.judge_failed += 1;
            "judge_failed"
        } else if drop {
            counts.dropped_by_judge += 1;
            if numbers {
                "unsupported_safety_fact"
            } else {
                "number_not_in_text"
            }
        } else if unsupported_extracted_safety {
            "shadow"
        } else if supported == Some(true) {
            "supported"
        } else {
            "not_a_safety_relation"
        };
        if p.is_some() {
            counts.judged += 1;
        }
        outcome.grounding.push(json!({
            "index": i,
            "memory_id": w.memory_id.to_string(),
            "source_kind": w.source_kind.as_str(),
            "subject": ef.subject,
            "predicate": ef.predicate,
            "object": ef.object,
            "claim": claim,
            "verdict": match supported {
                Some(true) => "supported",
                Some(false) => "unsupported",
                None => "not_judged",
            },
            // Three places: f32 widened to JSON prints 0.019999999552965164.
            "p": p.map(|p| (f64::from(p) * 1000.0).round() / 1000.0),
            "action": if drop { "dropped" } else { "written" },
            "reason": reason,
        }));
    }
    keep
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
/// Everything network-bound is paid for BEFORE the transaction opens: the
/// grounding call (see [`GroundingJudge`]), the embeddings (see
/// [`FactEmbedCache`]) and, under `--autonomy full`, every judge call (see
/// [`gather_judgements`]). The adjudication call used to await
/// INSIDE `BEGIN IMMEDIATE`, and with a provider's retry policy that held the
/// write lock for minutes — every concurrent `remember`, daemon job and
/// `facts resolve` then failed on busy_timeout.
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
    let mut scratch = FactWriteOutcome::default();
    // Grounding first: a dropped fact costs no embedding and no adjudication.
    let keep = ground_batch(ctx, w, batch, &mut scratch).await;
    let (facts, original): (Vec<ExtractedFact>, Vec<usize>) = batch
        .facts
        .iter()
        .zip(&keep)
        .enumerate()
        .filter(|(_, (_, keep))| **keep)
        .map(|(i, (ef, _))| (ef.clone(), i))
        .unzip();
    // Indices below are into `facts`; a failure names the fact by its place
    // in the batch the caller sent.
    let fail = |index: usize, error: CliError| FactBatchError {
        index: original.get(index).copied().unwrap_or(total),
        total,
        error,
    };

    let cache = match w.embedder {
        Some(embedder) => {
            let texts = batch_embed_texts(ctx, w, &facts);
            FactEmbedCache::warm(embedder, &texts)
                .await
                .map_err(|e| fail(0, e))?
        }
        None => FactEmbedCache::empty(),
    };

    let judged = match (w.autonomy, w.adjudicator) {
        (Autonomy::Full, Some(judge)) => {
            gather_judgements(ctx, w, &facts, &cache, judge, batch.source_text)
                .await
                .map_err(|(i, e)| fail(i, e))?
        }
        _ => Vec::new(),
    };

    ctx.store.begin_immediate().map_err(|e| fail(0, e.into()))?;
    let mut failed: Option<(usize, CliError)> = None;
    for (i, ef) in facts.iter().enumerate() {
        let verdict = judged.get(i).and_then(Option::as_ref);
        if let Err(e) = write_judged_fact(ctx, w, ef, &cache, verdict, &mut scratch).await {
            tracing::warn!("fact {} of {total} failed to persist: {e}", original[i] + 1);
            failed = Some((i, e));
            break;
        }
    }
    if failed.is_none() {
        if let Some(marker) = &batch.marker {
            // INVARIANT 1 HAS NO EXCEPTIONS [G6]. `record_fact_extraction`
            // mutates `fact_extraction_log` with no event behind it, so the
            // marker every normal extraction earned was a projection row
            // `admin rebuild` could not reconstruct: rebuild deletes the
            // derivation tables and refills them from `derivation.ran`, the
            // memory reappeared in the backfill queue, and the next sweep paid
            // a provider for a call the store had already made and thrown
            // away. `record_derivation` appends the event and writes both
            // markers in this same transaction.
            if let Err(e) = ctx.store.record_derivation(
                w.memory_id,
                marker.content_hash,
                engram_storage::journal::EXTRACTOR_FACT_LLM,
                marker.model,
                &scratch.fact_ids,
                &format!("cli:{}", marker.source),
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
    let (valid_until, until_note) = resolve_valid_until(ef, valid_from, state);
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

/// GATHER: every judge call a batch needs, made BEFORE the write lock is
/// taken. Each observation is planned read-only against the store as it is
/// now; the ones that would queue a routine pair are judged, one batched
/// request each.
///
/// The verdict is only a PROPOSAL. The apply pass plans again inside the
/// transaction — earlier facts of the same batch have landed by then — and
/// uses a verdict only for an observation whose queued pairs are exactly the
/// pairs that were judged (see [`JudgedPairs`]). Anything else stays queued.
///
/// Warnings and notes from this pass are thrown away: the apply pass reports
/// them, once.
async fn gather_judgements(
    ctx: &AppContext,
    w: &FactWriteCtx<'_>,
    facts: &[ExtractedFact],
    cache: &FactEmbedCache,
    judge: &dyn Judge,
    source_text: &str,
) -> Result<Vec<Option<Judged>>, (usize, CliError)> {
    let mut discarded = FactWriteOutcome::default();
    let mut out = Vec::with_capacity(facts.len());
    for (i, ef) in facts.iter().enumerate() {
        let prepared = prepare(ctx, w, ef, cache, &mut discarded)
            .await
            .map_err(|e| (i, e))?;
        let judged = match prepared {
            Prepared::Planned(p) if !p.plan.queue.is_empty() => {
                let by_id = p.by_id();
                // A safety pair is the caller's whatever a judge says, so it
                // is never sent: that would carry someone's medication list
                // to a third party for an answer nothing may use.
                let pairs: Vec<&Fact> = p
                    .plan
                    .queue
                    .iter()
                    .filter_map(|q| by_id.get(&q.old_fact_id).copied())
                    .filter(|old| !pair_is_safety(&p.observation, old))
                    .collect();
                if pairs.is_empty() {
                    None
                } else {
                    let judgement = judge_pairs(judge, source_text, &p.observation, &pairs).await;
                    if let Some(r) = &judgement.response {
                        record_judge_usage(
                            ctx,
                            JudgeSite::Adjudicate.operation(),
                            r,
                            Some(w.kb),
                            Some(w.diary),
                        );
                    }
                    Some(Judged {
                        about: JudgedPairs::of(&p.observation, &p.plan.queue),
                        judgement,
                    })
                }
            }
            _ => None,
        };
        out.push(judged);
    }
    Ok(out)
}

/// A pair no machine may settle: the observation or the stored fact is on a
/// safety relation.
fn pair_is_safety(observation: &Observation, old: &Fact) -> bool {
    observation.spec.is_safety() || is_safety_fact(old)
}

/// What [`conflict_rule`] makes of one pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairRuling {
    /// The new fact says what the stored one says: store nothing new.
    Restates,
    /// The new text says the stored value ended or was replaced.
    Supersede,
    /// Both hold: different attributes, or compatible values.
    Coexist,
    /// The signals do not settle it: the pair stays queued for the caller.
    Undecided,
}

impl PairRuling {
    fn word(self) -> &'static str {
        match self {
            PairRuling::Restates => "reject_new",
            PairRuling::Supersede => "supersede",
            PairRuling::Coexist => "coexist",
            PairRuling::Undecided => "undecided",
        }
    }
}

// The rule's cuts, fitted on a random half of the 148 labelled pairs
// (`benchmarks/jev/adjudicate_rule.py`, jev-1.13.0) and scored on the other
// half: 68/73 held-out against the prompted LLM's 63/73, 24/24 against 22/24
// on the pairs engram's planner can actually queue, rule >= LLM on 49 of 50
// random splits. They belong to that judge model: refit on a model change.

/// At or above: the new fact restates the stored one. High, because a false
/// yes throws the caller's fact away.
pub const RULE_RESTATES_P: f32 = 0.9;
/// Below: the judge is sure the two facts fill different attributes.
pub const RULE_OTHER_SLOT_P: f32 = 0.05;
/// At or above (with [`RULE_OLD_ENDED_P`]): the values cannot both hold.
pub const RULE_CONTRADICTS_P: f32 = 0.2;
/// At or above: the text says the STORED value ended. `contradicts` is
/// symmetric; this is the direction check, so an extractor that emitted the
/// newer value first cannot have it closed by the older one.
pub const RULE_OLD_ENDED_P: f32 = 0.1;
/// Below: the values are compatible and coexist.
pub const RULE_COMPATIBLE_P: f32 = 0.1;

/// The whole adjudication policy, in code: four calibrated probabilities in,
/// one ruling out. Nothing here knows about safety or authority; the applier
/// enforces both whatever this returns.
pub fn conflict_rule(a: &PairAnswers) -> PairRuling {
    if a.restates >= RULE_RESTATES_P {
        PairRuling::Restates
    } else if a.same_slot < RULE_OTHER_SLOT_P {
        PairRuling::Coexist
    } else if a.contradicts >= RULE_CONTRADICTS_P && a.old_ended >= RULE_OLD_ENDED_P {
        PairRuling::Supersede
    } else if a.contradicts < RULE_COMPATIBLE_P {
        PairRuling::Coexist
    } else {
        PairRuling::Undecided
    }
}

/// One judge's answers about ONE observation's routine queued pairs.
pub struct Judgement {
    /// Who answered, as the journal names it: `judge:<provider>`.
    pub judge: &'static str,
    pub model: String,
    /// Per stored fact asked about: its four signals, or `None` when the
    /// judge left any of them unanswered (that pair stays queued). `Err` =
    /// the call failed; every pair stays queued. The reason is a stable word.
    pub answers: Result<Vec<(Uuid, Option<PairAnswers>)>, &'static str>,
    /// For the usage ledger; never journaled.
    response: Option<engram_llm::judge::JudgeResponse>,
}

impl Judgement {
    /// The rule's ruling on one stored fact: `Undecided` for a pair that was
    /// not asked about, not answered, or whose call failed.
    fn ruling(&self, old_fact_id: Uuid) -> PairRuling {
        self.answers
            .as_ref()
            .ok()
            .and_then(|all| all.iter().find(|(id, _)| *id == old_fact_id))
            .and_then(|(_, a)| a.as_ref())
            .map_or(PairRuling::Undecided, conflict_rule)
    }

    /// Whether the judge was asked about this stored fact.
    fn asked(&self, old_fact_id: Uuid) -> bool {
        self.answers
            .as_ref()
            .map_or(true, |all| all.iter().any(|(id, _)| *id == old_fact_id))
    }
}

/// THE JUDGE SEAM: the memory text, the observation and the stored facts its
/// routine queued pairs are about, in; one batched JEV request (four nouls per
/// stored fact); answers out.
///
/// Called with no transaction open. Whatever it returns, the applier enforces
/// the same limits: a failure keeps every pair queued, a safety pair is never
/// cleared or discarded, a close needs `may_close`.
pub async fn judge_pairs(
    judge: &dyn Judge,
    source_text: &str,
    observation: &Observation,
    pairs: &[&Fact],
) -> Judgement {
    let new_fact = FactTriple::new(
        observation.subject.as_str(),
        observation.relation.as_str(),
        observation.object.as_str(),
    );
    let existing: Vec<(String, FactTriple)> = pairs
        .iter()
        .map(|f| {
            (
                f.id.to_string(),
                FactTriple::new(f.subject.as_str(), f.predicate.as_str(), f.object.as_str()),
            )
        })
        .collect();
    let request = pair_request(source_text, &new_fact, &existing);
    match ask_batched(judge, &request).await {
        Ok(response) => Judgement {
            judge: response.provider,
            model: response.model.clone(),
            answers: Ok(pairs
                .iter()
                .map(|f| {
                    let a = PairAnswers::from_response(&response, &f.id.to_string()).ok();
                    (f.id, a)
                })
                .collect()),
            response: Some(response),
        },
        Err(e) => {
            // Logged, never echoed into the envelope: the note only needs to
            // say that nobody decided.
            tracing::warn!("adjudication judge gave no answer; the pairs stay queued: {e}");
            Judgement {
                judge: judge.name(),
                model: judge.model_id().to_string(),
                answers: Err(failure_reason(&e)),
                response: None,
            }
        }
    }
}

/// What a verdict was ABOUT: the observation, and its queued pairs by old
/// fact, rule and semantic key. Fact ids of the observation itself are minted
/// per planning pass, so they are deliberately not part of it.
#[derive(Debug, PartialEq, Eq)]
struct JudgedPairs {
    subject_norm: String,
    relation: String,
    object_norm: String,
    pairs: Vec<(Uuid, &'static str, String)>,
}

impl JudgedPairs {
    fn of(observation: &Observation, queue: &[PlannedConflict]) -> JudgedPairs {
        let mut pairs: Vec<(Uuid, &'static str, String)> = queue
            .iter()
            .map(|q| (q.old_fact_id, q.verdict.rule, q.key.encode()))
            .collect();
        pairs.sort();
        JudgedPairs {
            subject_norm: observation.subject_norm.clone(),
            relation: observation.relation.clone(),
            object_norm: observation.object_norm.clone(),
            pairs,
        }
    }
}

/// A verdict gathered before the transaction, and what it was about.
struct Judged {
    about: JudgedPairs,
    judgement: Judgement,
}

/// One observation, planned against the store as it is now. Reads only.
enum Prepared {
    /// Not writable (a blank subject, relation or object) — silent on
    /// purpose: an extractor emitting a blank object is not a user-facing
    /// failure.
    Unwritable,
    /// The novelty gate found the same statement already stored.
    GateDuplicate(Uuid),
    Planned(Box<PreparedPlan>),
}

struct PreparedPlan {
    observation: Observation,
    siblings: Vec<Fact>,
    others: Vec<(Fact, f32)>,
    embedding: Option<Vec<f32>>,
    plan: WritePlan,
}

impl PreparedPlan {
    fn by_id(&self) -> HashMap<Uuid, &Fact> {
        self.siblings
            .iter()
            .chain(self.others.iter().map(|(f, _)| f))
            .map(|f| (f.id, f))
            .collect()
    }
}

/// Everything up to the plan, with no write: resolve the observation, load
/// its siblings, run the novelty gate, plan.
async fn prepare(
    ctx: &AppContext,
    w: &FactWriteCtx<'_>,
    ef: &ExtractedFact,
    cache: &FactEmbedCache,
    outcome: &mut FactWriteOutcome,
) -> Result<Prepared, CliError> {
    let Some(observation) = build_observation(ctx, w, ef, outcome) else {
        return Ok(Prepared::Unwritable);
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
            // The novelty gate answers "is this already stored about THIS
            // subject", so it only runs when nothing on this relation could
            // answer it. Its candidates are the subject's own open facts: a
            // `Duplicate` absorbs the new fact before the classifier runs, so
            // a candidate about someone else made Carol's medication a
            // confirmation of Bob's. It also keeps the scan to one subject's
            // vectors instead of decoding every open fact in the kb per fact.
            let emb = embed_fact_text(embedder, cache, &text).await?;
            let existing = if observation.queue_everything {
                // `ENGRAM_FACT_QUEUE_ALL` rebuilds the pre-triage write path
                // for `facts triage`'s tests, and that path's reach was the
                // whole kb: its cross-subject "conflicts" are part of the
                // backlog triage exists to drain.
                ctx.store
                    .active_fact_embeddings(diary, kb, &embedder.model())?
            } else {
                ctx.store.open_fact_embeddings_for_subject(
                    diary,
                    kb,
                    &observation.subject_norm,
                    &embedder.model(),
                )?
            };
            let (dup_t, novel_t) = gate_thresholds()?;
            outcome.gate.ran += 1;
            match novelty_gate(&emb, &existing, dup_t, novel_t) {
                GateDecision::Duplicate { fact_id, score } => {
                    let candidate = ctx.store.get_fact(fact_id)?;
                    if !observation.spec.is_safety()
                        && !candidate.as_ref().is_some_and(is_safety_fact)
                    {
                        outcome.gate.duplicate += 1;
                        return Ok(Prepared::GateDuplicate(fact_id));
                    }
                    // An absorb is a heuristic discarding the caller's
                    // observation, and the candidate is by construction on
                    // ANOTHER relation: the gate only runs when this one has
                    // no facts. "Allergic to penicillin" vanished as a
                    // restatement of "takes penicillin". A safety fact is
                    // never retired or discarded by a heuristic (IN-19), so
                    // the classifier decides, as in the uncertain band.
                    outcome.gate.uncertain += 1;
                    outcome.gate.uncertain_candidates += 1;
                    others = candidate.map(|f| (f, score)).into_iter().collect();
                }
                GateDecision::Novel => outcome.gate.novel += 1,
                GateDecision::Uncertain { candidates } => {
                    outcome.gate.uncertain += 1;
                    outcome.gate.uncertain_candidates += candidates.len();
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
            outcome.gate.skipped_has_siblings += 1;
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
    let plan = plan_write(
        &observation,
        &siblings,
        &others,
        classify_mode_for(w.source_kind),
        &prior,
    )?;
    Ok(Prepared::Planned(Box::new(PreparedPlan {
        observation,
        siblings,
        others,
        embedding,
        plan,
    })))
}

/// Write one fact inside the caller's transaction. Never calls a judge:
/// `compile` uses this directly, and under `--autonomy full` the judging
/// happens in [`write_fact_batch`], before its transaction opens.
pub async fn write_fact(
    ctx: &AppContext,
    w: &FactWriteCtx<'_>,
    ef: &ExtractedFact,
    cache: &FactEmbedCache,
    outcome: &mut FactWriteOutcome,
) -> Result<(), CliError> {
    write_judged_fact(ctx, w, ef, cache, None, outcome).await
}

/// APPLY: plan again, inside the transaction, and use a gathered verdict only
/// if it is still about exactly this observation's queued pairs.
async fn write_judged_fact(
    ctx: &AppContext,
    w: &FactWriteCtx<'_>,
    ef: &ExtractedFact,
    cache: &FactEmbedCache,
    judged: Option<&Judged>,
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
    let (mut plan, p) = match prepare(ctx, w, ef, cache, outcome).await? {
        Prepared::Planned(p) => {
            let mut p = *p;
            (std::mem::take(&mut p.plan), p)
        }
        early => {
            if let Some(j) = judged {
                settle_judgement(ctx, w, j, None, outcome)?;
            }
            if let Prepared::GateDuplicate(fact_id) = early {
                ctx.store.increment_fact_confirmations(fact_id)?;
                outcome.deduped += 1;
            }
            return Ok(());
        }
    };
    let applied = match judged {
        Some(j) => settle_judgement(ctx, w, j, Some((&mut plan, &p)), outcome)?,
        None => false,
    };
    let by_id = p.by_id();
    let routine_queued = plan.queue.iter().any(|q| {
        by_id
            .get(&q.old_fact_id)
            .is_some_and(|old| !pair_is_safety(&p.observation, old))
    });
    if !applied && w.autonomy == Autonomy::Full && w.adjudicator.is_some() && routine_queued {
        // The store moved between judging and writing (an earlier fact of
        // this batch landed), so no verdict answers THESE pairs. Either the
        // judge answered different pairs, or it was never asked: the pairs
        // did not exist yet when the batch was judged.
        plan.notes.push(if judged.is_some() {
            PlanNote::AdjudicationStale
        } else {
            outcome.adjudication.not_asked += 1;
            PlanNote::AdjudicationNotAsked
        });
    }

    apply_plan(
        ctx,
        w,
        &p.observation,
        &plan,
        &p.siblings,
        &p.others,
        p.embedding.as_deref(),
        outcome,
    )
}

/// One gathered verdict, handled exactly once: counted, journaled, and
/// applied if it still answers this observation's queued pairs. `planned` is
/// `None` when the observation no longer plans at all (the gate now finds it
/// stored). Returns whether the verdict was applied.
fn settle_judgement(
    ctx: &AppContext,
    w: &FactWriteCtx<'_>,
    judged: &Judged,
    planned: Option<(&mut WritePlan, &PreparedPlan)>,
    outcome: &mut FactWriteOutcome,
) -> Result<bool, CliError> {
    let counts = &mut outcome.adjudication;
    counts.calls += 1;
    let j = &judged.judgement;
    match &j.answers {
        Err(_) => counts.failed += 1,
        Ok(answers) => {
            counts.pairs_in += answers.len();
            for (id, _) in answers {
                match j.ruling(*id) {
                    PairRuling::Supersede => counts.supersede += 1,
                    PairRuling::Coexist => counts.coexist += 1,
                    PairRuling::Restates => counts.reject_new += 1,
                    PairRuling::Undecided => {}
                }
            }
        }
    }
    let fresh = planned.filter(|(plan, p)| {
        !plan.queue.is_empty() && judged.about == JudgedPairs::of(&p.observation, &plan.queue)
    });
    record_judgement(ctx, w, judged, fresh.is_none())?;
    let Some((plan, p)) = fresh else {
        counts.stale += 1;
        return Ok(false);
    };
    let closes = plan.closes.len();
    apply_judgement(plan, &p.observation, &p.by_id(), j);
    counts.pairs_closed += plan.closes.len().saturating_sub(closes);
    counts.pairs_kept += plan.queue.len();
    Ok(true)
}

/// `--autonomy full`: let [`conflict_rule`] settle what the classifier
/// queued, pair by pair.
///
/// The rule is a `Heuristic` authority and nothing more. A safety pair was
/// never sent to the judge and stays queued; the caller (who has the
/// conversation) decides it. A `Supersede` closes only what `may_close`
/// allows. A `Restates` discards the observation only when no safety pair is
/// queued with it. An `Undecided` pair stays queued.
///
/// A failed call is not an error: every pair stays queued, which is what
/// `assisted` would have done anyway, and the envelope says so
/// (`adjudication_failed`).
fn apply_judgement(
    plan: &mut WritePlan,
    observation: &Observation,
    by_id: &HashMap<Uuid, &Fact>,
    judgement: &Judgement,
) {
    if let Err(reason) = judgement.answers {
        plan.notes.push(PlanNote::AdjudicationFailed { reason });
        return;
    }
    let is_safety = |q: &PlannedConflict| {
        observation.spec.is_safety() || by_id.get(&q.old_fact_id).is_some_and(|f| is_safety_fact(f))
    };
    if plan
        .queue
        .iter()
        .any(|q| judgement.ruling(q.old_fact_id) == PairRuling::Restates)
    {
        if plan.queue.iter().any(is_safety) {
            // Dropping the observation would dispose of the safety question
            // queued with it: the same outcome as a close, by another door.
            plan.notes.push(PlanNote::AdjudicationKeptSafety {
                action: "reject_new",
            });
        } else {
            // The planner's own absorb, not a hand-rolled half of it: a
            // close, confirmation or replay planned on behalf of an
            // observation that is then discarded would retire a value on the
            // word of a fact that never reaches the store.
            plan.absorb("semantic_jev");
        }
        return;
    }
    let authority = CloseAuthority::Heuristic {
        rule: "semantic_jev",
    };
    let mut refused = false;
    let mut kept: Vec<PlannedConflict> = Vec::new();
    for q in std::mem::take(&mut plan.queue) {
        if is_safety(&q) || !judgement.asked(q.old_fact_id) {
            kept.push(q);
            continue;
        }
        match judgement.ruling(q.old_fact_id) {
            PairRuling::Coexist => {}
            PairRuling::Supersede => {
                let allowed = by_id
                    .get(&q.old_fact_id)
                    .map(|f| relation_spec(&canonical_predicate(&f.predicate)))
                    .is_some_and(|spec| may_close(&spec, &authority) == CloseVerdict::Allow);
                if allowed {
                    plan.closes.push(PlannedClose {
                        fact_id: q.old_fact_id,
                        at: observation.interval.from,
                        reason: CloseReason::Superseded,
                        authority: authority.clone(),
                        closed_by: Some(observation.fact_id),
                        resolution_tag: "semantic_jev",
                    });
                } else {
                    refused = true;
                    kept.push(q);
                }
            }
            PairRuling::Restates | PairRuling::Undecided => kept.push(q),
        }
    }
    plan.queue = kept;
    if refused {
        plan.notes.push(PlanNote::AdjudicationKeptSafety {
            action: "supersede",
        });
    }
}

/// Journal WHICH judge answered and WHAT it said: one `void` machine decision
/// per pair it was asked about, on the existing `decision.recorded` event, so
/// it survives `admin rebuild` (the close's `resolution` tag does not). The
/// note carries the ruling and the four probabilities, which is what a later
/// refit of the rule's cuts reads.
///
/// `void` because the row must record without binding. `lookup_decision`
/// never selects a void and `replay` treats one as not binding, so the next
/// write on the same key is planned exactly as if this row did not exist.
///
/// A stale verdict (judged against a store that moved before the write)
/// keeps the judge's real answer and adds `applied=false`: it was paid for
/// and answered, just not used, and the audit trail says what it was.
fn record_judgement(
    ctx: &AppContext,
    w: &FactWriteCtx<'_>,
    judged: &Judged,
    stale: bool,
) -> Result<(), CliError> {
    let j = &judged.judgement;
    let applied = if stale { " applied=false" } else { "" };
    let actor = format!("judge:{}", j.judge);
    for (old_fact_id, rule, key) in &judged.about.pairs {
        if !j.asked(*old_fact_id) {
            continue;
        }
        let action = match j.answers {
            Err(_) => "failed",
            Ok(_) => j.ruling(*old_fact_id).word(),
        };
        let signals = j
            .answers
            .as_ref()
            .ok()
            .and_then(|all| all.iter().find(|(id, _)| id == old_fact_id))
            .and_then(|(_, a)| a.as_ref())
            .map(|a| {
                format!(
                    " restates={} same_slot={} contradicts={} old_ended={}",
                    a.restates, a.same_slot, a.contradicts, a.old_ended
                )
            })
            .unwrap_or_default();
        let note = format!(
            "action={action}{applied} model={} rule={rule}{signals}",
            j.model
        );
        ctx.store.record_decision(
            w.kb,
            w.diary,
            key,
            "void",
            None,
            "machine",
            &actor,
            Some(&note),
            None,
            SEMANTIC_KEY_VERSION,
        )?;
    }
    Ok(())
}

/// Why a judge gave no verdict, as a stable word for the envelope.
fn failure_reason(e: &JudgeError) -> &'static str {
    match e {
        JudgeError::Transport { .. } | JudgeError::Status { .. } => "judge_unreachable",
        JudgeError::InvalidResponse { .. }
        | JudgeError::MissingAnswer { .. }
        | JudgeError::WrongAnswerType { .. } => "not_a_verdict",
        JudgeError::InvalidRequest(_) => "invalid_request",
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
    // The PLAN owns the interval, not the observation: a cessation with
    // nothing to close is dated from the end it states, not from the memory
    // that reported it.
    let inserted = plan.insert.as_ref().map(|planned| {
        let mut fact = observation.to_fact(Utc::now());
        fact.valid_from = Some(planned.interval.from);
        fact
    });
    if let (Some(fact), Some(planned)) = (inserted.as_ref(), plan.insert.as_ref()) {
        ctx.store.insert_fact(fact)?;
        if let (Some(emb), Some(embedder)) = (embedding, w.embedder) {
            ctx.store
                .set_fact_embedding(fact.id, emb, &embedder.model())?;
        }
        outcome.added += 1;
        // A cessation with nothing open to close is born and then closed, so
        // the journal carries a reason instead of a mute pre-dead row. The
        // audit tag comes from the AUTHORITY, never from a literal: the row
        // used to stamp itself `explicit_end` even when an extraction model
        // was the only thing claiming the interval had ended [G1].
        if let Some((reason, authority)) = &planned.close {
            let at = planned.interval.until.unwrap_or(planned.interval.from);
            let tag = resolution_tag(w.source_kind, end_resolution_tag(authority));
            ctx.store.close_fact_interval(
                fact.id,
                at,
                reason.as_str(),
                authority.as_str(),
                None,
                Some(tag),
            )?;
        }
        outcome.fact_ids.push(fact.id);
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
    *outcome
        .queued_by_rule
        .entry(planned.verdict.rule)
        .or_default() += 1;
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

    #[test]
    fn a_gate_threshold_nobody_can_use_is_a_config_error_not_the_default() {
        let defaults = (
            engram_graph::gate::DEFAULT_DUP_THRESHOLD,
            engram_graph::gate::DEFAULT_NOVEL_THRESHOLD,
        );
        assert_eq!(parse_gate_thresholds(None, None).unwrap(), defaults);
        assert_eq!(
            parse_gate_thresholds(Some(" "), Some("")).unwrap(),
            defaults
        );
        assert_eq!(
            parse_gate_thresholds(Some("0.9"), Some("-0.2")).unwrap(),
            (0.9, -0.2)
        );
        for (dup, novel) in [
            (Some("abc"), None),
            (None, Some("0.7x")),
            (Some("1.5"), None),
            (Some("NaN"), None),
            (None, Some("-2")),
            (Some("0.5"), Some("0.5")),
            (Some("0.3"), None),
        ] {
            let err = parse_gate_thresholds(dup, novel).unwrap_err();
            assert!(matches!(err, CliError::Config(_)), "{dup:?} {novel:?}");
            assert_eq!(err.exit_code(), 2);
        }
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
        let (until, note) =
            resolve_valid_until(&ef, Some(moment("2026-01-01")), ObservationState::Ongoing);
        assert_eq!(until.unwrap(), moment("2026-06-01"));
        assert!(note.is_none());
        assert!(ef.says_ended());
    }

    #[test]
    fn an_unreadable_end_date_is_dropped_and_reported_never_guessed() {
        // A guessed end date closes an interval that is still running — the
        // safety failure, arriving through the date parser.
        let ef = ending("takes_medication", "metformin", Some("some time last year"));
        let (until, note) =
            resolve_valid_until(&ef, Some(moment("2026-01-01")), ObservationState::Ongoing);
        assert_eq!(until, None);
        assert!(matches!(note, Some(PlanNote::UnreadableValidUntil { .. })));
    }

    #[test]
    fn an_end_before_the_start_is_dropped_not_stored_backwards() {
        let ef = ending("lives_in", "Lisbon", Some("2019-01-01"));
        let (until, note) =
            resolve_valid_until(&ef, Some(moment("2026-01-01")), ObservationState::Ongoing);
        assert_eq!(until, None);
        assert!(matches!(
            note,
            Some(PlanNote::ValidUntilBeforeValidFrom { .. })
        ));
    }

    #[test]
    fn no_end_date_is_not_a_warning() {
        let ef = extracted("lives_in", "Lisbon", None);
        let (until, note) =
            resolve_valid_until(&ef, Some(moment("2026-01-01")), ObservationState::Ongoing);
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

    // ----- the judge seam fails closed -----

    /// A judge that is never reachable: a timeout, a 401, exhausted retries.
    struct Unreachable;

    impl Judge for Unreachable {
        fn name(&self) -> &'static str {
            "unreachable"
        }

        fn model_id(&self) -> &str {
            "unreachable-model"
        }

        fn ask<'life0, 'life1, 'async_trait>(
            &'life0 self,
            _request: &'life1 engram_llm::judge::JudgeRequest,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<
                        Output = Result<engram_llm::judge::JudgeResponse, JudgeError>,
                    > + Send
                    + 'async_trait,
            >,
        >
        where
            'life0: 'async_trait,
            'life1: 'async_trait,
            Self: 'async_trait,
        {
            Box::pin(async {
                Err(JudgeError::Status {
                    provider: "unreachable",
                    status: 503,
                    message: "down".into(),
                })
            })
        }
    }

    fn observation(relation: &str, object: &str) -> Observation {
        let relation = relation.to_string();
        let object_norm = normalize(object);
        Observation {
            fact_id: Uuid::new_v4(),
            subject: "Ana".into(),
            subject_norm: "ana".into(),
            spec: relation_spec(&relation),
            member: member_identity(&relation, &object_norm),
            relation,
            object: object.into(),
            object_norm,
            confidence: 1.0,
            source_memory_id: Uuid::new_v4(),
            source_kind: SourceKind::UserStated,
            fact_type: FactType::Assertion,
            diary: "default".into(),
            kb: "personal".into(),
            interval: Interval {
                from: moment("2026-01-01"),
                until: None,
            },
            state: ObservationState::Ongoing,
            autonomy: Autonomy::Full,
            queue_everything: false,
        }
    }

    fn metformin(object: &str) -> Observation {
        observation("takes_medication", object)
    }

    /// A routine pair the planner queues: two cities out of ONE memory.
    fn same_source_cities() -> (Observation, Fact, WritePlan) {
        let mut old_obs = observation("lives_in", "Lisbon");
        let mut new = observation("lives_in", "Berlin");
        old_obs.source_memory_id = new.source_memory_id;
        new.interval.from = moment("2026-01-01");
        let old = old_obs.to_fact(moment("2026-01-01"));
        let prior = |_: &SemanticKey| Ok(None);
        let plan = plan_write(
            &new,
            std::slice::from_ref(&old),
            &[],
            ClassifyMode::Write,
            &prior,
        )
        .unwrap();
        assert_eq!(plan.queue.len(), 1, "one text, two cities: a question");
        (new, old, plan)
    }

    fn answered(old: &Fact, a: PairAnswers) -> Judgement {
        Judgement {
            judge: "stub",
            model: "stub".into(),
            answers: Ok(vec![(old.id, Some(a))]),
            response: None,
        }
    }

    fn signals(restates: f32, same_slot: f32, contradicts: f32, old_ended: f32) -> PairAnswers {
        PairAnswers {
            restates,
            same_slot,
            contradicts,
            old_ended,
        }
    }

    #[test]
    fn the_rule_maps_signals_to_the_four_outcomes() {
        use PairRuling::*;
        // Measured on the pair set: a move, a correction, a restatement.
        assert_eq!(conflict_rule(&signals(0.02, 0.97, 0.78, 0.86)), Supersede);
        assert_eq!(conflict_rule(&signals(0.01, 0.95, 0.81, 0.98)), Supersede);
        assert_eq!(conflict_rule(&signals(0.97, 0.98, 0.05, 0.02)), Restates);
        // A second language: compatible, both kept.
        assert_eq!(conflict_rule(&signals(0.02, 0.84, 0.05, 0.03)), Coexist);
        // Confidently different attributes.
        assert_eq!(conflict_rule(&signals(0.01, 0.02, 0.5, 0.5)), Coexist);
        // They clash but the text never says the STORED value ended: the
        // direction is unknown, so the caller decides.
        assert_eq!(conflict_rule(&signals(0.02, 0.95, 0.6, 0.05)), Undecided);
        // Weak clash, nothing ended: the caller decides.
        assert_eq!(conflict_rule(&signals(0.02, 0.93, 0.15, 0.3)), Undecided);
    }

    #[tokio::test]
    async fn an_unreachable_judge_gives_no_verdict_and_every_pair_survives() {
        let (new, old, mut plan) = same_source_cities();
        let before = plan.queue.clone();

        let judgement = judge_pairs(&Unreachable, "Ana lives in Berlin.", &new, &[&old]).await;
        assert_eq!(judgement.answers, Err("judge_unreachable"));
        assert_eq!(judgement.model, "unreachable-model");

        let by_id: HashMap<Uuid, &Fact> = HashMap::from([(old.id, &old)]);
        apply_judgement(&mut plan, &new, &by_id, &judgement);
        assert_eq!(plan.queue, before, "no verdict decides nothing");
        assert!(plan.insert.is_some() && plan.closes.is_empty());
        assert_eq!(
            plan.notes,
            [PlanNote::AdjudicationFailed {
                reason: "judge_unreachable"
            }]
        );
    }

    #[test]
    fn a_routine_supersede_closes_through_may_close() {
        let (new, old, mut plan) = same_source_cities();
        let by_id: HashMap<Uuid, &Fact> = HashMap::from([(old.id, &old)]);
        let j = answered(&old, signals(0.02, 0.97, 0.78, 0.86));
        apply_judgement(&mut plan, &new, &by_id, &j);
        assert!(plan.queue.is_empty());
        assert_eq!(plan.closes.len(), 1);
        assert_eq!(plan.closes[0].fact_id, old.id);
        assert_eq!(plan.closes[0].resolution_tag, "semantic_jev");
    }

    #[test]
    fn an_undecided_or_unanswered_pair_stays_queued() {
        let (new, old, mut plan) = same_source_cities();
        let before = plan.queue.clone();
        let by_id: HashMap<Uuid, &Fact> = HashMap::from([(old.id, &old)]);
        apply_judgement(
            &mut plan,
            &new,
            &by_id,
            &answered(&old, signals(0.02, 0.95, 0.6, 0.05)),
        );
        assert_eq!(plan.queue, before);
        let unanswered = Judgement {
            judge: "stub",
            model: "stub".into(),
            answers: Ok(vec![(old.id, None)]),
            response: None,
        };
        apply_judgement(&mut plan, &new, &by_id, &unanswered);
        assert_eq!(plan.queue, before);
        assert!(plan.closes.is_empty() && plan.absorbed.is_none());
    }

    #[test]
    fn no_ruling_closes_clears_or_discards_a_safety_pair() {
        let old = metformin("metformin 500 mg").to_fact(moment("2025-01-01"));
        let new = metformin("metformin 1000 mg");
        let prior = |_: &SemanticKey| Ok(None);
        let by_id: HashMap<Uuid, &Fact> = HashMap::from([(old.id, &old)]);
        for a in [
            signals(0.01, 0.97, 0.95, 0.97),
            signals(0.01, 0.97, 0.01, 0.01),
            signals(0.99, 0.99, 0.01, 0.01),
            signals(0.01, 0.01, 0.01, 0.01),
        ] {
            let mut plan = plan_write(
                &new,
                std::slice::from_ref(&old),
                &[],
                ClassifyMode::Write,
                &prior,
            )
            .unwrap();
            let before = plan.queue.clone();
            assert_eq!(before.len(), 1, "a dose change is a question");
            apply_judgement(&mut plan, &new, &by_id, &answered(&old, a));
            assert_eq!(plan.queue, before, "{a:?}");
            assert!(plan.closes.is_empty() && plan.absorbed.is_none(), "{a:?}");
            assert!(plan.insert.is_some(), "{a:?}");
        }
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

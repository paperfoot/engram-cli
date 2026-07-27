//! Write-path v2 orchestration: semantic supersession + cooperative
//! adjudication.
//!
//! Per extracted fact:
//! 1. canonicalize the predicate (synonym map) and the subject (alias/fuzzy
//!    cascade against known subjects), then normalize;
//! 2. exact path — an active fact with the same (subject_norm, predicate):
//!    same object → dedup (confirmation++); different object → the conflict
//!    classifier decides (see below);
//! 3. semantic path — the embedding novelty gate classifies against all
//!    active facts: duplicate → confirmation++; novel → insert; uncertain →
//!    the classifier again, and only what it cannot settle is escalated by
//!    autonomy mode: `full` asks this system's LLM, `assisted` (default)
//!    inserts the new fact as *contested* and queues the conflict for the
//!    calling agent, `strict` always queues.
//!
//! The caller is a frontier agent with conversation context this store
//! cannot see — `assisted` mode hands it the adjudication instead of paying
//! for a worse-informed LLM call here.
//!
//! CONFLICT CLASSIFICATION (`classify_pair`) is the gate in front of the
//! queue. Importing 1,332 real memories produced 2,953 facts and 9,510
//! queued "conflicts" — a backlog no human will ever adjudicate, which
//! buries the handful of contradictions that matter. Almost none of them
//! were contradictions: candidates about a different person, candidates on a
//! different predicate, and second values of multi-valued predicates
//! (`knows`, `friend_of`, `met_at`) which are additions, not disagreements.
//! The classifier resolves those without queueing and reserves the queue for
//! same-slot contradictions with no recency ordering — plus every safety
//! slot, which is never auto-superseded.

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

// ===========================================================================
// Conflict classification
// ===========================================================================

/// How many values one `(subject, predicate)` slot may hold at once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arity {
    /// One value at a time — a new value replaces the old one.
    Single,
    /// Many values coexist — a second value is an addition, not a conflict.
    Multi,
}

/// Canonical predicates that are single-valued but carry no slot policy of
/// their own. `slot_policy` covers the profile/safety slots; these are the
/// remaining "a person has exactly one" attributes.
const SINGLE_VALUED_EXTRA: &[&str] = &[
    "died_on",
    "birthday",
    "has_birthday",
    "date_of_birth",
    "nationality",
    "has_nationality",
];

/// Canonical predicates that are explicitly multi-valued. The DEFAULT for an
/// unknown predicate is also multi-valued — this list documents the ones that
/// dominate a personal store (networking, family, events) and pins them in a
/// test, because treating them as slots is what turned a person's whole
/// address book into a conflict queue.
pub const MULTI_VALUED_PREDICATES: &[&str] = &[
    "knows",
    "friend_of",
    "friends_with",
    "met_at",
    "met",
    "family_of",
    "related_to",
    "sibling_of",
    "parent_of",
    "child_of",
    "colleague_of",
    "worked_with",
    "mentor_of",
    "introduced_by",
    "introduced_to",
    "invested_in",
    "founded",
    "owns",
    "is_member_of",
    "attended",
    "visited",
    "speaks",
    "prefers",
    "likes",
    "dislikes",
    "interested_in",
    "works_on",
];

/// Single- vs multi-valued. Unknown predicates default to MULTI: the cost of
/// a wrong `Multi` is two coexisting facts, the cost of a wrong `Single` is a
/// true fact silently superseded or a human asked to adjudicate a non-event.
pub fn predicate_arity(canonical: &str) -> Arity {
    if slot_policy(canonical).is_some() || SINGLE_VALUED_EXTRA.contains(&canonical) {
        Arity::Single
    } else {
        Arity::Multi
    }
}

/// Safety-critical slots (allergies, medication, diagnoses, blood type). A
/// fact on one of these is never auto-superseded, at write time or in a bulk
/// triage — a wrong answer here is a medical incident, not a stale profile
/// field. The only thing either path may do unattended is drop a queued pair
/// that is about two DIFFERENT people, which supersedes nothing.
pub fn is_safety_slot(canonical: &str) -> bool {
    matches!(slot_policy(canonical), Some(SlotPolicy::AwaitConfirmation))
}

/// What to do with one `(new fact, old fact)` pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictClass {
    /// Not a conflict: both facts are true at the same time.
    Coexist,
    /// Same claim: keep the existing fact, count a confirmation.
    Duplicate,
    /// Single-valued slot, the new value is the current one.
    SupersedeOld,
    /// Single-valued slot, the INCOMING value is the stale one.
    SupersedeNew,
    /// One of the pair is already superseded — nothing left to decide.
    Settled,
    /// A human (or the calling agent) must decide.
    Queue,
}

/// A classification plus the rule that produced it. The rule name is reported
/// in `facts triage` output so a bulk auto-resolution is auditable rather
/// than a number the user has to trust.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Verdict {
    pub class: ConflictClass,
    pub rule: &'static str,
}

impl Verdict {
    const fn new(class: ConflictClass, rule: &'static str) -> Self {
        Verdict { class, rule }
    }
    pub fn is_auto_resolvable(&self) -> bool {
        !matches!(self.class, ConflictClass::Queue)
    }
}

/// Which side of the fact layer is asking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClassifyMode {
    /// The write path: an agent is asserting this value right now, so
    /// ingestion order is itself evidence (last writer wins the slot).
    Write,
    /// `facts triage`: re-reading a backlog where ingestion order proves
    /// nothing, so only an explicit event-time ordering (or a wide confidence
    /// gap) resolves a slot. Everything else about the rules is identical to
    /// `Write` — same function, same safety guarantees.
    Triage,
}

/// Event times closer together than this are "the same moment" — clock noise
/// and same-batch writes must not read as a correction.
const RECENCY_MARGIN_SECS: i64 = 1;

/// Confidence gap above which the more-confident value wins a single-valued
/// slot outright instead of asking a human (see `classify_pair`).
const CONFIDENCE_GAP: f32 = 0.3;

/// Cosine above which two DIFFERENT predicates on the same subject are
/// treated as the same slot phrased two ways ("lives_in" vs "home_base_is").
/// Below it they are simply two different attributes.
const PARAPHRASE_SIMILARITY: f32 = 0.88;

/// Event time of a fact: when it became true, falling back to when the store
/// learned it.
fn effective_time(f: &Fact) -> DateTime<Utc> {
    f.valid_from.unwrap_or(f.created_at)
}

/// Classify a candidate conflict between an incoming/newer fact and an
/// existing one. Pure and side-effect free so the write path and `facts
/// triage` apply exactly the same rules — that identity is what makes triage
/// "resolve everything the new rules would never have queued" true by
/// construction rather than by a second, drifting implementation.
///
/// `score` is the novelty gate's cosine when the pair came from the semantic
/// path, `None` for exact (same subject + predicate) pairs.
pub fn classify_pair(new: &Fact, old: &Fact, score: Option<f32>, mode: ClassifyMode) -> Verdict {
    let new_pred = canonical_predicate(&new.predicate);
    let old_pred = canonical_predicate(&old.predicate);
    let safety = is_safety_slot(&new_pred) || is_safety_slot(&old_pred);
    let same_subject = new.subject_norm == old.subject_norm;

    if !same_subject {
        // Two people can hold opposite values without contradicting anyone.
        // This verdict never touches a fact — both stay active, the pair is
        // simply not a question — so it is safe even for a safety predicate:
        // one person's allergy is not evidence about another's.
        return Verdict::new(ConflictClass::Coexist, "different_subject");
    }
    // One subject, a safety slot involved: the only class that may ever come
    // out of here is Queue. A wrong answer about an allergy, a medication or
    // a diagnosis is a medical incident, so neither the write path nor a bulk
    // triage is allowed to pick a winner — a human confirms it.
    if safety {
        return Verdict::new(ConflictClass::Queue, "safety_slot");
    }
    if new.superseded_by.is_some() || old.superseded_by.is_some() {
        return Verdict::new(ConflictClass::Settled, "already_settled");
    }
    if new_pred != old_pred {
        // Same subject, different predicates: only a near-identical phrasing
        // can be the same slot in disguise, and even then a human decides —
        // superseding across predicates would rewrite a claim nobody made.
        let paraphrase = score.unwrap_or(0.0) >= PARAPHRASE_SIMILARITY
            && predicate_arity(&new_pred) == Arity::Single
            && predicate_arity(&old_pred) == Arity::Single;
        return if paraphrase {
            Verdict::new(ConflictClass::Queue, "paraphrase_slot")
        } else {
            Verdict::new(ConflictClass::Coexist, "different_predicate")
        };
    }
    if new.object_norm == old.object_norm {
        return Verdict::new(ConflictClass::Duplicate, "duplicate_object");
    }
    if predicate_arity(&new_pred) == Arity::Multi {
        // `Ana knows Bob` does not stop being true when `Ana knows Carol`
        // arrives. This single rule is most of the 9,510.
        return Verdict::new(ConflictClass::Coexist, "multi_valued");
    }

    // Single-valued, non-safety, same slot, different values: a real
    // disagreement. Resolve it when the evidence orders the two values.
    let (nt, ot) = (effective_time(new), effective_time(old));
    let gap = (nt - ot).num_seconds();
    let same_source = new.source_memory_id == old.source_memory_id;
    if !same_source && gap >= RECENCY_MARGIN_SECS {
        let rule = match mode {
            // Preserve the long-standing audit tags on the write path.
            ClassifyMode::Write => write_supersede_rule(&new_pred),
            ClassifyMode::Triage => "recency_supersede",
        };
        return Verdict::new(ConflictClass::SupersedeOld, rule);
    }
    if !same_source && gap <= -RECENCY_MARGIN_SECS {
        // The incoming value is older than what is stored (a backfill of an
        // old memory, or an out-of-order import): the stored value stands.
        return Verdict::new(ConflictClass::SupersedeNew, "recency_stale");
    }
    if same_source {
        // Both values came out of one text: ordering them by clock is
        // meaningless, so this is exactly the contradiction a human wants.
        return Verdict::new(ConflictClass::Queue, "same_source_contradiction");
    }
    // Comparable event times. The write path still has ingestion order to go
    // on — an agent asserting a value now is asserting the current one.
    if mode == ClassifyMode::Write {
        return Verdict::new(ConflictClass::SupersedeOld, write_supersede_rule(&new_pred));
    }
    let conf_gap = new.confidence - old.confidence;
    if conf_gap >= CONFIDENCE_GAP {
        return Verdict::new(ConflictClass::SupersedeOld, "confidence_gap");
    }
    if conf_gap <= -CONFIDENCE_GAP {
        return Verdict::new(ConflictClass::SupersedeNew, "confidence_gap");
    }
    Verdict::new(ConflictClass::Queue, "comparable_values")
}

/// Audit tag for a write-path supersession, unchanged from write-path v2:
/// `slot_lww` for a declared profile slot, `exact_match` otherwise.
fn write_supersede_rule(canonical: &str) -> &'static str {
    match slot_policy(canonical) {
        Some(SlotPolicy::LastWriterWins) => "slot_lww",
        _ => "exact_match",
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
    let (valid_from, date_warning) = resolve_valid_from(ef, &predicate, w.memory_time);
    if let Some(warning) = date_warning {
        // Recorded before any dedup/queue branch: the date was unreadable
        // whatever happens to the fact itself, and the caller is the only one
        // who can fix the source.
        outcome
            .date_warnings
            .push(warning.to_json(&subject, &predicate));
    }

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
        // Same question, different answer — or, for a multi-valued predicate,
        // simply a second answer. The classifier decides per pair.
        insert_with_embedding(ctx, &new_fact, embedder, kb, diary).await?;
        outcome.added += 1;
        for old in &exact {
            apply_write_verdict(
                ctx, &new_fact, old, diary, kb, "slot", None, autonomy, outcome,
            )?;
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
            // Classify BEFORE escalating. The gate's uncertain band is a
            // similarity band, not a contradiction: most candidates are about
            // someone else or about a different attribute of the same person.
            // Filtering here is what stops a personal store from queueing
            // three "conflicts" per imported fact.
            let mut cand_facts: Vec<(Fact, f32)> = cand_facts;
            let mut settled: Vec<(Fact, f32)> = Vec::new();
            if !queue_everything() {
                cand_facts.retain(|(f, s)| {
                    let keep = classify_pair(&new_fact, f, Some(*s), ClassifyMode::Write).class
                        == ConflictClass::Queue;
                    if !keep {
                        settled.push((f.clone(), *s));
                    }
                    keep
                });
            }
            if cand_facts.is_empty() {
                // Nothing here a human or an LLM could improve on.
                ctx.store.insert_fact(&new_fact)?;
                ctx.store
                    .set_fact_embedding(new_fact.id, &embedding, &embedder.model())?;
                outcome.added += 1;
                for (f, s) in &settled {
                    apply_write_verdict(
                        ctx,
                        &new_fact,
                        f,
                        diary,
                        kb,
                        "semantic_cosine",
                        Some(*s),
                        autonomy,
                        outcome,
                    )?;
                }
                return Ok(());
            }
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

/// Classify one candidate pair and carry out the verdict. `new_fact` is
/// already in the store; the classifier decides whether the pair is a
/// supersession, a non-event, or something only the caller can settle.
#[allow(clippy::too_many_arguments)]
fn apply_write_verdict(
    ctx: &AppContext,
    new_fact: &Fact,
    old: &Fact,
    diary: &str,
    kb: &str,
    detector: &str,
    score: Option<f32>,
    autonomy: Autonomy,
    outcome: &mut FactWriteOutcome,
) -> Result<(), CliError> {
    let verdict = classify_pair(new_fact, old, score, ClassifyMode::Write);
    // `strict` promised never to auto-supersede: it escalates every real
    // disagreement. It does NOT invent conflicts out of coexisting values —
    // a second `knows` is not a decision anyone has to make.
    let class = match (verdict.class, autonomy) {
        (ConflictClass::SupersedeOld | ConflictClass::SupersedeNew, Autonomy::Strict) => {
            ConflictClass::Queue
        }
        (ConflictClass::Duplicate, _) => ConflictClass::Duplicate,
        (c, _) if queue_everything() => {
            let _ = c;
            ConflictClass::Queue
        }
        (c, _) => c,
    };
    match class {
        ConflictClass::Coexist | ConflictClass::Settled => {}
        ConflictClass::Duplicate => {
            // The exact path dedupes before inserting, so this only fires if
            // a duplicate slipped through another route: keep the original
            // row (its id is what other tables reference) and count a
            // confirmation instead of leaving two identical active facts.
            ctx.store
                .supersede_fact_with(new_fact.id, old.id, "duplicate", None)?;
            ctx.store.increment_fact_confirmations(old.id)?;
            outcome.deduped += 1;
            outcome.added = outcome.added.saturating_sub(1);
        }
        ConflictClass::SupersedeOld => {
            ctx.store.supersede_fact_with(
                old.id,
                new_fact.id,
                verdict.rule,
                new_fact.valid_from,
            )?;
            outcome
                .conflicts
                .push(conflict_payload(old, new_fact, verdict.rule));
        }
        ConflictClass::SupersedeNew => {
            ctx.store
                .supersede_fact_with(new_fact.id, old.id, verdict.rule, old.valid_from)?;
            outcome
                .conflicts
                .push(conflict_payload(new_fact, old, verdict.rule));
        }
        ConflictClass::Queue => {
            queue_conflict(ctx, new_fact, old, diary, kb, detector, score, outcome)?;
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

    // ----- Conflict classification -----

    /// A stored fact. `at` is the event time; every fact gets its own source
    /// memory unless the caller pins one, because "came out of the same text"
    /// is itself a classification signal.
    fn stored(subject: &str, predicate: &str, object: &str, at: &str) -> Fact {
        let t = moment(at);
        Fact {
            id: Uuid::new_v4(),
            source_memory_id: Uuid::new_v4(),
            subject: subject.into(),
            subject_norm: normalize(subject),
            predicate: predicate.into(),
            object: object.into(),
            object_norm: normalize(object),
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
        }
    }

    fn triage(new: &Fact, old: &Fact) -> Verdict {
        classify_pair(new, old, None, ClassifyMode::Triage)
    }

    #[test]
    fn arity_defaults_to_multi_and_knows_the_slots() {
        assert_eq!(predicate_arity("lives_in"), Arity::Single);
        assert_eq!(predicate_arity("works_at"), Arity::Single);
        assert_eq!(predicate_arity("has_role"), Arity::Single);
        assert_eq!(predicate_arity("born_on"), Arity::Single);
        assert_eq!(predicate_arity("allergic_to"), Arity::Single);
        for p in MULTI_VALUED_PREDICATES {
            assert_eq!(predicate_arity(p), Arity::Multi, "{p} must be multi-valued");
        }
        // Unknown predicates are multi-valued: a wrong Single silently hides
        // a true fact, a wrong Multi only keeps both.
        assert_eq!(predicate_arity("collaborated_with"), Arity::Multi);
    }

    #[test]
    fn second_value_of_a_multi_valued_predicate_is_not_a_conflict() {
        let old = stored("Elias", "knows", "Alice", "2026-01-01");
        let new = stored("Elias", "knows", "Bob", "2026-02-01");
        let v = triage(&new, &old);
        assert_eq!(v.class, ConflictClass::Coexist);
        assert_eq!(v.rule, "multi_valued");
        // ...and the same holds on the write path, in every autonomy mode.
        assert_eq!(
            classify_pair(&new, &old, None, ClassifyMode::Write).class,
            ConflictClass::Coexist
        );
    }

    #[test]
    fn different_subjects_never_conflict() {
        let old = stored("Alice", "lives_in", "Berlin", "2026-01-01");
        let new = stored("Bob", "lives_in", "Porto", "2026-02-01");
        assert_eq!(triage(&new, &old).rule, "different_subject");
        assert_eq!(triage(&new, &old).class, ConflictClass::Coexist);
    }

    #[test]
    fn different_predicates_on_one_subject_coexist_unless_they_are_one_slot() {
        let old = stored("Elias", "lives_in", "Lisbon", "2026-01-01");
        let new = stored("Elias", "has_role", "CEO", "2026-02-01");
        assert_eq!(triage(&new, &old).rule, "different_predicate");
        // A near-identical embedding of two single-valued predicates is the
        // paraphrase case the novelty gate exists for — a human decides,
        // because superseding across predicates rewrites a claim nobody made.
        let old = stored("Elias", "lives_in", "Lisbon", "2026-01-01");
        let new = stored("Elias", "home_city_is", "Porto", "2026-02-01");
        assert_eq!(
            classify_pair(&new, &old, Some(0.93), ClassifyMode::Triage).class,
            ConflictClass::Coexist,
            "an unknown predicate is multi-valued, so it cannot claim a slot"
        );
        let old = stored("Elias", "lives_in", "Lisbon", "2026-01-01");
        let new = stored("Elias", "born_on", "1980-01-01", "2026-02-01");
        let v = classify_pair(&new, &old, Some(0.93), ClassifyMode::Triage);
        assert_eq!(v.class, ConflictClass::Queue);
        assert_eq!(v.rule, "paraphrase_slot");
        // Below the paraphrase band the two are simply different attributes.
        assert_eq!(
            classify_pair(&new, &old, Some(0.70), ClassifyMode::Triage).rule,
            "different_predicate"
        );
    }

    #[test]
    fn identical_objects_are_duplicates_not_contradictions() {
        let old = stored("Elias", "works_at", "Acme", "2026-01-01");
        let new = stored("Elias", "works_at", "acme", "2026-02-01");
        let v = triage(&new, &old);
        assert_eq!(v.class, ConflictClass::Duplicate);
        assert_eq!(v.rule, "duplicate_object");
    }

    #[test]
    fn safety_slots_are_never_auto_resolved_for_one_subject() {
        let old = stored("Ana", "allergic_to", "penicillin", "2020-01-01");
        let new = stored("Ana", "allergic_to", "amoxicillin", "2026-01-01");
        // Six years of recency ordering is not enough: this stays queued.
        assert_eq!(triage(&new, &old).class, ConflictClass::Queue);
        assert_eq!(triage(&new, &old).rule, "safety_slot");
        assert_eq!(
            classify_pair(&new, &old, None, ClassifyMode::Write).class,
            ConflictClass::Queue
        );
        // Any predicate paired with a safety slot on the same subject queues.
        let other = stored("Ana", "takes_medication", "amoxicillin", "2026-01-01");
        assert_eq!(triage(&other, &old).rule, "safety_slot");
        // Different people never contradict each other, and un-queueing that
        // pair supersedes nothing — no allergy can be hidden by it.
        let bob = stored("Bob", "allergic_to", "peanuts", "2026-01-01");
        assert_eq!(triage(&bob, &old).rule, "different_subject");
        assert_eq!(triage(&bob, &old).class, ConflictClass::Coexist);
    }

    #[test]
    fn a_clear_event_time_ordering_resolves_a_slot_in_both_directions() {
        let old = stored("Elias", "lives_in", "London", "2015-06-01");
        let new = stored("Elias", "lives_in", "Lisbon", "2024-02-01");
        let v = triage(&new, &old);
        assert_eq!(v.class, ConflictClass::SupersedeOld);
        assert_eq!(v.rule, "recency_supersede");
        // Backfilling an OLD memory must not overwrite the current value.
        let v = triage(&old, &new);
        assert_eq!(v.class, ConflictClass::SupersedeNew);
        assert_eq!(v.rule, "recency_stale");
    }

    #[test]
    fn two_values_from_one_memory_are_a_real_contradiction() {
        let mut old = stored("Carol", "works_at", "Acme", "2026-01-01");
        let mut new = stored("Carol", "works_at", "Beta Corp", "2026-01-01");
        new.source_memory_id = Uuid::new_v4();
        old.source_memory_id = new.source_memory_id;
        // Even with a clock gap: one text, two employers, no ordering.
        new.valid_from = Some(moment("2026-06-01"));
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
        // The write path has one more piece of evidence — an agent is
        // asserting this value right now — and keeps write-path v2's tag.
        let v = classify_pair(&new, &old, None, ClassifyMode::Write);
        assert_eq!(v.class, ConflictClass::SupersedeOld);
        assert_eq!(v.rule, "slot_lww");
    }

    #[test]
    fn a_wide_confidence_gap_settles_a_slot_without_a_human() {
        let old = stored("Elias", "works_at", "Acme", "2026-01-01");
        let mut new = stored("Elias", "works_at", "Beta Corp", "2026-01-01");
        new.created_at = old.created_at;
        new.confidence = 1.0;
        let mut old_weak = old.clone();
        old_weak.confidence = 0.4;
        let v = triage(&new, &old_weak);
        assert_eq!(v.class, ConflictClass::SupersedeOld);
        assert_eq!(v.rule, "confidence_gap");
        // A guess must not overwrite a confident value either.
        let v = triage(&old_weak, &new);
        assert_eq!(v.class, ConflictClass::SupersedeNew);
        assert_eq!(v.rule, "confidence_gap");
    }

    #[test]
    fn an_already_superseded_fact_leaves_nothing_to_decide() {
        let mut old = stored("Elias", "works_at", "Acme", "2026-01-01");
        let new = stored("Elias", "works_at", "Beta Corp", "2026-02-01");
        old.superseded_by = Some(Uuid::new_v4());
        let v = triage(&new, &old);
        assert_eq!(v.class, ConflictClass::Settled);
        assert_eq!(v.rule, "already_settled");
        assert!(v.is_auto_resolvable());
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
            assert_eq!(
                Verdict::new(class, "test").is_auto_resolvable(),
                expected,
                "{class:?}"
            );
        }
    }
}

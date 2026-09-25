//! What a second value MEANS — the classifier, the decision ledger's replay
//! half, and the pure write planner.
//!
//! v0.3.1 derived arity from "does this predicate have a slot policy", so
//! `takes_medication` was a single-valued slot and five concurrent medications
//! produced ten pairwise "conflicts", each suggesting the previous drug be
//! superseded. That is the polypharmacy storm, and it dies here: the registry
//! (`engram_core::relations`) says a medication is a SET, and a second member
//! of a set is an addition. Different members never form a pair at all.
//!
//! Three things live in this module and nowhere else:
//!
//! 1. **Contradiction**, defined once. `classify_pair` is the only place a
//!    disagreement is named, so the write path, `facts triage` and `engram
//!    state` cannot drift apart on what a conflict is.
//! 2. **The suggestion table** (R7). `supersede-old` is never advised on a
//!    safety pair and never as a laundering of classifier uncertainty — but it
//!    IS advised where the classifier ordered the values and only
//!    `--autonomy strict` put the pair in the queue. Suppressing an answer the
//!    classifier reached is as dishonest as inventing one it did not.
//! 3. **The pure planner** (`plan_write`). The write path splits into a
//!    planner that touches no store and an applier that does nothing else, so
//!    the polypharmacy guarantee is provable from fixtures rather than from a
//!    SQLite fixture and a hope.
//!
//! The registry and the interval algebra are NOT here: they live in
//! `engram_core::{relations, interval}` because the v9→v10 migration keys
//! ~14.5k decision rows through them and `engram-core` cannot depend on
//! `engram-graph` (interfaces.md R1). This module owns what they MEAN.

use chrono::{DateTime, Utc};
use engram_core::interval::{
    close_at, is_open_fact, may_close, CloseAuthority, CloseReason, CloseVerdict, Interval,
};
use engram_core::keys;
use engram_core::relations::{
    canonical_relation, member_identity, relation_spec, Mutability, RelationSpec,
    MEMBER_IDENTITY_VERSION, REGISTRY_VERSION,
};
use engram_core::types::{Fact, FactType, SourceKind};
use engram_storage::{DecisionRow, SqliteStore, StorageError};
use serde_json::json;
use uuid::Uuid;

pub use engram_core::keys::SEMANTIC_KEY_VERSION;

/// Event times closer together than this are "the same moment" — clock noise
/// and same-batch writes must not read as a correction.
pub const RECENCY_MARGIN_SECS: i64 = 1;

/// Confidence gap above which the more-confident value wins a single-valued
/// slot outright instead of asking a human.
pub const CONFIDENCE_GAP: f32 = 0.3;

/// Cosine above which two DIFFERENT relations on one subject are treated as
/// the same slot phrased two ways ("lives_in" vs "home_base_is").
pub const PARAPHRASE_SIMILARITY: f32 = 0.88;

/// The heuristic rule a cessation the EXTRACTOR claimed is closed under, and
/// the queued verdict's rule name when the safety gate refuses that close.
pub const EXTRACTOR_CESSATION: &str = "extractor_cessation";

/// Canonical relations that are explicitly set-valued. The DEFAULT for an
/// unknown relation is also set-valued — this list documents the ones that
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

// ===========================================================================
// Autonomy
// ===========================================================================

/// How much authority this process has over conflict resolution.
///
/// It lives here rather than in the CLI because the planner applies it: a
/// policy that lived in the applier could assert a close the applier then
/// refused, and two policies in two places is how `strict` came to mean
/// something slightly different at each call site.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Autonomy {
    /// Adjudicate uncertain conflicts with our own LLM call.
    Full,
    /// Queue uncertain conflicts for the calling agent (default).
    #[default]
    Assisted,
    /// Never auto-supersede anything; queue every real disagreement.
    Strict,
}

/// The one autonomy spelling nobody recognised, carried as a type so the CLI
/// can map it to its own error without this crate knowing what a CLI is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownAutonomy(pub String);

impl std::fmt::Display for UnknownAutonomy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "unknown autonomy mode '{}' (expected full|assisted|strict)",
            self.0
        )
    }
}

impl Autonomy {
    pub fn parse(s: &str) -> Result<Autonomy, UnknownAutonomy> {
        match s {
            "full" => Ok(Autonomy::Full),
            "assisted" => Ok(Autonomy::Assisted),
            "strict" => Ok(Autonomy::Strict),
            other => Err(UnknownAutonomy(other.to_string())),
        }
    }
}

// ===========================================================================
// Classification
// ===========================================================================

/// What to do with one `(new fact, old fact)` pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictClass {
    /// Not a conflict: both facts are true at the same time.
    Coexist,
    /// Same claim: keep the existing interval, count a confirmation.
    Duplicate,
    /// The OLD interval closes — it is not deleted, it gains a `valid_until`.
    SupersedeOld,
    /// The INCOMING interval closes on arrival: it is the stale one.
    SupersedeNew,
    /// One of the pair is already settled — nothing left to decide.
    Settled,
    /// A human (or the calling agent) must decide.
    Queue,
}

impl ConflictClass {
    pub fn as_str(self) -> &'static str {
        match self {
            ConflictClass::Coexist => "coexist",
            ConflictClass::Duplicate => "duplicate",
            ConflictClass::SupersedeOld => "supersede_old",
            ConflictClass::SupersedeNew => "supersede_new",
            ConflictClass::Settled => "settled",
            ConflictClass::Queue => "queue",
        }
    }
}

/// A classification, the rule that produced it, and — when the verdict was
/// escalated to `Queue` — the class it held before.
///
/// `escalated_from` is not bookkeeping: R7's one honest `supersede-old`
/// suggestion is not computable without it. The planner's strict-autonomy step
/// turns a close into a queued question and would otherwise throw away the
/// answer the classifier reached, leaving `suggested_action` unable to tell
/// "queued because strict" from "queued because nobody could order these".
/// `safety_gate` sets it too, which is exactly why arm (1) and arm (3) of the
/// suggestion table can never both match [D18].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Verdict {
    pub class: ConflictClass,
    pub rule: &'static str,
    pub escalated_from: Option<ConflictClass>,
}

impl Verdict {
    pub const fn new(class: ConflictClass, rule: &'static str) -> Self {
        Verdict {
            class,
            rule,
            escalated_from: None,
        }
    }

    /// The same rule, re-classified as a question, remembering what it was.
    pub const fn escalated(class: ConflictClass, rule: &'static str, from: ConflictClass) -> Self {
        Verdict {
            class,
            rule,
            escalated_from: Some(from),
        }
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
    /// gap) resolves a slot. Every other rule is identical.
    Triage,
}

/// Everything the classifier needs about ONE side of a pair, resolved once so
/// the registry and the member heuristic are not re-run per comparison.
#[derive(Debug, Clone)]
pub struct FactView<'a> {
    pub fact: &'a Fact,
    /// Canonical relation — never the raw predicate.
    pub relation: String,
    pub spec: RelationSpec,
    /// `member_identity(relation, object_norm)`: which SET MEMBER this names.
    pub member: String,
    pub interval: Interval,
}

impl<'a> FactView<'a> {
    pub fn of(f: &'a Fact) -> FactView<'a> {
        let relation = canonical_relation(&f.predicate);
        let spec = relation_spec(&relation);
        let member = member_identity(&relation, &f.object_norm);
        FactView {
            fact: f,
            relation,
            spec,
            member,
            interval: Interval::of(f),
        }
    }

    fn is_safety(&self) -> bool {
        self.spec.is_safety()
    }
}

/// Classify one candidate pair. Pure, side-effect free, and the ONLY place
/// contradiction is defined.
///
/// `score` is the novelty gate's cosine when the pair came from the semantic
/// path, `None` for exact (same subject + relation) pairs.
pub fn classify_pair(new: &Fact, old: &Fact, score: Option<f32>, mode: ClassifyMode) -> Verdict {
    classify_views(&FactView::of(new), &FactView::of(old), score, mode)
}

/// Same, without re-deriving the views — the hot path is one new fact against
/// N siblings, and the member heuristic is not free.
pub fn classify_views(
    new: &FactView<'_>,
    old: &FactView<'_>,
    score: Option<f32>,
    mode: ClassifyMode,
) -> Verdict {
    // 1. Two people can hold opposite values without contradicting anyone.
    //    This verdict touches no row, so it is safe even on a safety relation.
    if new.fact.subject_norm != old.fact.subject_norm {
        return Verdict::new(ConflictClass::Coexist, "different_subject");
    }
    // 2. Ahead of the safety check, deliberately. In v0.3.1 the safety
    //    early-return fired first, so a safety pair whose loser had already
    //    been retired stayed in the queue forever with nothing left to decide.
    if new.fact.superseded_by.is_some() || old.fact.superseded_by.is_some() {
        return Verdict::new(ConflictClass::Settled, "already_settled");
    }
    // 3. Different relations: only a near-identical phrasing can be the same
    //    slot in disguise, and even then a human decides — superseding across
    //    relations rewrites a claim nobody made.
    if new.relation != old.relation {
        let paraphrase = score.unwrap_or(0.0) >= PARAPHRASE_SIMILARITY
            && new.spec.is_single_valued()
            && old.spec.is_single_valued();
        return if paraphrase {
            Verdict::new(ConflictClass::Queue, "paraphrase_slot")
        } else {
            Verdict::new(ConflictClass::Coexist, "different_predicate")
        };
    }
    safety_gate(same_relation(new, old, mode), new, old)
}

/// Steps 4–8: one subject, one relation.
fn same_relation(new: &FactView<'_>, old: &FactView<'_>, mode: ClassifyMode) -> Verdict {
    // 4. The same value said twice.
    if new.fact.object_norm == old.fact.object_norm {
        return match old.interval.until {
            None => Verdict::new(ConflictClass::Duplicate, "duplicate_object"),
            // Restarting a medication after stopping it is a second interval,
            // not a duplicate and not a contradiction.
            Some(until) if new.interval.from >= until => {
                Verdict::new(ConflictClass::Coexist, "reinstated_interval")
            }
            // A backdated assertion INSIDE an already-closed window is not
            // news: the store already recorded that the state held then, and
            // re-opening it would resurrect a value someone deliberately
            // ended. Without this arm the pair falls through to the interval
            // rules and can be closed or queued on the strength of a clock.
            Some(_) => Verdict::new(ConflictClass::Settled, "restates_closed_interval"),
        };
    }
    if new.spec.is_set_valued() {
        // 5. Different members of a set. THIS RUNS BEFORE ANY SAFETY
        //    CONSIDERATION — the inversion that produced ten queued pairs from
        //    five medications. In practice the write path never even builds
        //    these pairs; this rule mostly serves the backlog drain.
        if new.member != old.member {
            return if new.is_safety() || old.is_safety() {
                Verdict::new(ConflictClass::Coexist, "safety_set_coexist")
            } else {
                Verdict::new(ConflictClass::Coexist, "multi_valued")
            };
        }
        // 6. Same member, different object — a dose change.
        if new.is_safety() || old.is_safety() {
            return Verdict::new(ConflictClass::Queue, "safety_member_change");
        }
        if ordered_by_event_time(new, old) {
            return Verdict::new(ConflictClass::SupersedeOld, "member_attribute_change");
        }
        return ordering_rules(new, old, mode);
    }
    // 7. Single-valued and immutable: no clock settles two birthdays.
    if new.spec.mutability == Mutability::Immutable {
        let corrected = mode == ClassifyMode::Write
            && matches!(
                new.fact.source_kind,
                SourceKind::UserStated | SourceKind::UserConfirmed
            )
            && matches!(
                old.fact.source_kind,
                SourceKind::AgentExtracted | SourceKind::CompileLlm | SourceKind::Import
            );
        return if corrected {
            // A person correcting their own birthday is the one ordering
            // signal an immutable attribute has. It does not survive the
            // safety gate on `blood_type`, which is deliberate.
            Verdict::new(ConflictClass::SupersedeOld, "correction_by_user")
        } else {
            Verdict::new(ConflictClass::Queue, "immutable_attribute_disagreement")
        };
    }
    // 8. Single-valued, mutable — the interval rules.
    ordering_rules(new, old, mode)
}

/// True when the two intervals start far enough apart, from different source
/// memories, for the later one to be evidence about the earlier one.
fn ordered_by_event_time(new: &FactView<'_>, old: &FactView<'_>) -> bool {
    new.fact.source_memory_id != old.fact.source_memory_id
        && (new.interval.from - old.interval.from).num_seconds() >= RECENCY_MARGIN_SECS
}

/// The ordering half of step 8, shared with step 6's unordered fall-through.
fn ordering_rules(new: &FactView<'_>, old: &FactView<'_>, mode: ClassifyMode) -> Verdict {
    // London 2015–2020 and Lisbon 2024–now are both true; superseding one
    // loses the history the interval model exists to keep.
    if !new.interval.overlaps(&old.interval) {
        return Verdict::new(ConflictClass::Coexist, "disjoint_intervals");
    }
    let gap = (new.interval.from - old.interval.from).num_seconds();
    let same_source = new.fact.source_memory_id == old.fact.source_memory_id;
    if !same_source && gap >= RECENCY_MARGIN_SECS {
        let rule = match mode {
            ClassifyMode::Write => write_supersede_rule(&new.relation),
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
    if mode == ClassifyMode::Write {
        return Verdict::new(
            ConflictClass::SupersedeOld,
            write_supersede_rule(&new.relation),
        );
    }
    let conf_gap = new.fact.confidence - old.fact.confidence;
    if conf_gap >= CONFIDENCE_GAP {
        return Verdict::new(ConflictClass::SupersedeOld, "confidence_gap");
    }
    if conf_gap <= -CONFIDENCE_GAP {
        return Verdict::new(ConflictClass::SupersedeNew, "confidence_gap");
    }
    Verdict::new(ConflictClass::Queue, "comparable_values")
}

/// Defence in depth, exported so no caller re-implements it: downgrade any
/// interval-closing verdict on a safety relation to `Queue`.
///
/// `Duplicate` is in here because `Duplicate` is a WRITE in the backlog sweep,
/// not a no-op — `facts triage` resolves it by superseding the incoming row.
/// The write planner absorbs a duplicate by CONFIRMING instead, so no safety
/// re-assertion becomes a question there; this gate is what stops the sweep
/// from retiring a safety row behind a duplicate verdict.
///
/// The rule name stays `safety_slot` on purpose: `commands/facts.rs` reads
/// `kept_by_reason["safety_slot"]` to populate `safety_slots_kept`, and a
/// rename would silently report zero.
pub fn safety_gate(v: Verdict, new: &FactView<'_>, old: &FactView<'_>) -> Verdict {
    if !(new.is_safety() || old.is_safety()) {
        return v;
    }
    match v.class {
        ConflictClass::SupersedeOld | ConflictClass::SupersedeNew | ConflictClass::Duplicate => {
            Verdict::escalated(ConflictClass::Queue, "safety_slot", v.class)
        }
        _ => v,
    }
}

/// The rules whose queued verdict means the caller has to look at the pair
/// themselves — either nobody could order the values, or the store holds an
/// answer it cannot map onto them.
const UNORDERABLE_RULES: &[&str] = &[
    "same_source_contradiction",
    "comparable_values",
    "paraphrase_slot",
    "immutable_attribute_disagreement",
    "replay_loser_unmatched",
];

/// What the CALLER should be told to do. Never `supersede-old` on a safety
/// pair, and never when the classifier could not order the values (R7).
///
/// Four arms, in this order. Arm (3) is R7's honest case and the only
/// surviving `supersede-old` suggestion: the classifier DID order the two
/// values and the pair is queued solely because `--autonomy strict` refused to
/// act on that ordering. Telling the caller what the classifier concluded is
/// not laundering uncertainty — there is no uncertainty to launder.
pub fn suggested_action(v: &Verdict, new: &FactView<'_>, old: &FactView<'_>) -> &'static str {
    if new.is_safety() || old.is_safety() {
        return "requires_context";
    }
    if UNORDERABLE_RULES.contains(&v.rule) {
        return "requires_context";
    }
    if v.escalated_from == Some(ConflictClass::SupersedeOld) && new.spec.is_single_valued() {
        return "supersede-old";
    }
    "keep-both"
}

/// The actions `facts resolve` will accept for this pair.
///
/// Every pair, safety included: a caller decision IS `CloseAuthority::Caller`,
/// so permission is not narrowed — only advice. `keep-both` stays first
/// because `respond.rs` runs element 0 in the ready-to-run command.
pub fn allowed_actions(
    _v: &Verdict,
    _new: &FactView<'_>,
    _old: &FactView<'_>,
) -> &'static [&'static str] {
    &["keep-both", "supersede-old", "reject-new"]
}

/// Audit tag for a write-path supersession: `slot_lww` for a single-valued
/// relation, `exact_match` otherwise. Re-expressed against the registry rather
/// than the deprecated `slot_policy` shim, which now answers differently for
/// the safety sets.
pub fn write_supersede_rule(canonical: &str) -> &'static str {
    if relation_spec(canonical).is_single_valued() {
        "slot_lww"
    } else {
        "exact_match"
    }
}

/// Which rule set a writer gets.
///
/// Ingestion order is evidence only when an agent is asserting a value NOW.
/// A COMPILED fact is a re-reading of text already in the store, compiled in
/// `ORDER BY position, id` — a UUID tiebreak. Letting that decide which of two
/// papers holds a slot is a coin toss with an audit trail, so a compiled fact
/// gets exactly the rules `facts triage` applies to a backlog.
pub fn classify_mode_for(source_kind: SourceKind) -> ClassifyMode {
    match source_kind {
        SourceKind::CompileLlm => ClassifyMode::Triage,
        _ => ClassifyMode::Write,
    }
}

/// Audit tag written into `facts.resolution` when the COMPILER settles a slot.
/// "A paper the compiler read disagreed with an older paper" is a different
/// claim from "the user told us so", so it gets its own namespace.
pub fn compile_resolution(rule: &'static str) -> &'static str {
    match rule {
        "recency_supersede" => "compile_recency",
        "recency_stale" => "compile_stale",
        "confidence_gap" => "compile_confidence",
        other => other,
    }
}

// ===========================================================================
// Semantic keys and decision replay
// ===========================================================================

/// The identity of a QUESTION, independent of the fact rows that asked it.
/// A re-derived fact with a fresh UUID after `edit --content` hits the same
/// key as the fact it replaced, which is the whole point of decision replay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticKey {
    pub subject_norm: String,
    pub relation: String,
    pub object_pair_hash: String,
}

impl SemanticKey {
    /// Over the two facts' `object_norm`s, NOT their member identities: the
    /// v9→v10 migration has only `object_norm`, and the migration and the
    /// classifier must agree or 14.5k migrated decisions stop replaying.
    pub fn for_pair(new: &FactView<'_>, old: &FactView<'_>) -> SemanticKey {
        SemanticKey {
            subject_norm: new.fact.subject_norm.clone(),
            relation: new.relation.clone(),
            object_pair_hash: keys::object_pair_hash(&new.fact.object_norm, &old.fact.object_norm),
        }
    }

    pub fn encode(&self) -> String {
        keys::semantic_key_encode(&self.subject_norm, &self.relation, &self.object_pair_hash)
    }

    pub fn decode(s: &str) -> Option<SemanticKey> {
        let (subject_norm, relation, object_pair_hash) = keys::semantic_key_decode(s)?;
        Some(SemanticKey {
            subject_norm,
            relation,
            object_pair_hash,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecisionVerdict {
    /// `facts resolve --action keep-both`, or a coexistence verdict.
    KeepBoth,
    /// `--action supersede-old`: the older interval closes.
    CloseOld,
    /// `--action reject-new`: the incoming interval closes.
    RejectNew,
    /// The question stopped existing without being answered: its facts were
    /// cascaded away, or a migration could not reconstruct a verdict. Recorded
    /// so the row's history survives, and NEVER replayed.
    Void,
}

impl DecisionVerdict {
    pub fn as_str(self) -> &'static str {
        match self {
            DecisionVerdict::KeepBoth => "keep_both",
            DecisionVerdict::CloseOld => "close_old",
            DecisionVerdict::RejectNew => "reject_new",
            DecisionVerdict::Void => "void",
        }
    }

    /// Anything unrecognised is `Void`: a verdict nobody can read must not
    /// replay as a guess.
    pub fn parse(s: &str) -> DecisionVerdict {
        match s {
            "keep_both" => DecisionVerdict::KeepBoth,
            "close_old" => DecisionVerdict::CloseOld,
            "reject_new" => DecisionVerdict::RejectNew,
            _ => DecisionVerdict::Void,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecisionAuthority {
    /// A human or the calling agent, via `facts resolve`.
    Caller,
    /// A classifier sweep (`facts triage`, the write path, the compiler).
    Machine,
}

impl DecisionAuthority {
    pub fn as_str(self) -> &'static str {
        match self {
            DecisionAuthority::Caller => "caller",
            DecisionAuthority::Machine => "machine",
        }
    }

    /// Anything that is not literally `caller` is a machine. Guessing the
    /// other way would let a free-text actor string authorize a safety close.
    pub fn parse(s: &str) -> DecisionAuthority {
        match s {
            "caller" => DecisionAuthority::Caller,
            _ => DecisionAuthority::Machine,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecisionRecord {
    /// The ledger row id — `decision.replayed` names it, so it is not
    /// optional the way spec-fact-semantics §2.3.2 implied.
    pub id: Uuid,
    pub key: SemanticKey,
    pub verdict: DecisionVerdict,
    /// The `object_norm` of the value this decision RETIRED [G3]. The semantic
    /// key identifies the QUESTION and is deliberately pair-order-insensitive;
    /// this field identifies the ANSWER. `None` = a verdict with no loser
    /// (`keep_both`, `void`), or a pre-v12 row whose conflict row was already
    /// gone when the migration swept for one.
    pub loser_object_norm: Option<String>,
    pub authority: DecisionAuthority,
    pub actor: String,
    pub decided_at: DateTime<Utc>,
    pub note: Option<String>,
    /// The queue row this decision answered, when it answered one. Carried so
    /// a pre-G3 decision's orientation can still be recovered from the pair it
    /// was taken on.
    pub conflict_id: Option<Uuid>,
    pub key_version: String,
}

impl DecisionRecord {
    /// Storage owns the row shape (interfaces §3.4, FROZEN); this is the whole
    /// adapter.
    pub fn from_row(r: DecisionRow) -> DecisionRecord {
        DecisionRecord {
            id: r.id,
            key: SemanticKey {
                subject_norm: r.subject_norm,
                relation: r.relation,
                object_pair_hash: r.pair_hash,
            },
            verdict: DecisionVerdict::parse(&r.verdict),
            // G3's storage half landed in schema v12, so this is the recorded
            // orientation and not a placeholder. `None` means NOT RECORDED —
            // a row written before v12, or a verdict that retired nothing —
            // and `binding_decision` then tries `recover_loser` before `replay`
            // decides what an unprovable orientation may do.
            loser_object_norm: r.loser_object_norm,
            authority: DecisionAuthority::parse(&r.authority),
            actor: r.actor,
            decided_at: r.decided_at,
            note: r.note,
            conflict_id: r.conflict_id,
            key_version: r.key_version,
        }
    }
}

/// The store, behind a trait so replay is unit-testable without SQLite.
///
/// The method is `binding_decision`, NOT `lookup_decision`: `SqliteStore` has
/// an inherent `lookup_decision`, and an inherent method shadows a trait
/// method of the same name at every call site. One name, one meaning.
pub trait DecisionLedger {
    fn binding_decision(
        &self,
        kb: &str,
        key: &SemanticKey,
    ) -> Result<Option<DecisionRecord>, StorageError>;
}

/// The adapter over the landed `lookup_decision`, whose query already
/// implements the precedence (newest caller row, else newest machine row;
/// `void` never selected), plus the orientation recovery a pre-G3 row needs.
impl DecisionLedger for SqliteStore {
    fn binding_decision(
        &self,
        kb: &str,
        key: &SemanticKey,
    ) -> Result<Option<DecisionRecord>, StorageError> {
        let Some(row) = self.lookup_decision(kb, &key.encode())? else {
            return Ok(None);
        };
        let mut record = DecisionRecord::from_row(row);
        if record.loser_object_norm.is_none() {
            record.loser_object_norm = recover_loser(self, &record)?;
        }
        Ok(Some(record))
    }
}

/// THE LEGACY PATH ONLY: which value a decision retired, recovered from the
/// queue row it answered.
///
/// `fact_decisions` carried no orientation before schema v12, and the semantic
/// key cannot supply one — it hashes the object pair in sorted order because it
/// names the QUESTION. The conflict row knows: a `close_old` retired its
/// `old_fact_id`, a `reject_new` its `new_fact_id`. Reading it here means a
/// ruling taken before the column existed still lands on the value the caller
/// actually rejected, instead of on whichever side of the pair happens to
/// arrive second in the next re-derivation.
///
/// This is now a second line of defence rather than the only one: the v12
/// migration backfills the same answer from the same row while the store is
/// being upgraded, precisely because the queue row is destructible (a `kb
/// delete` cascades it through the fact foreign keys while the decision
/// survives). An orientation that lives only here is one the store can be made
/// to forget. This path still runs for a row the migration could not reach —
/// a decision recorded against a conflict that was purged before the upgrade.
///
/// Every absence answers `None` — no conflict id, a cascaded queue row, a
/// deleted fact. Recovery is best effort; it never invents an orientation, and
/// [`replay`] decides what an unprovable one is allowed to do.
fn recover_loser(
    store: &SqliteStore,
    record: &DecisionRecord,
) -> Result<Option<String>, StorageError> {
    if !matches!(
        record.verdict,
        DecisionVerdict::CloseOld | DecisionVerdict::RejectNew
    ) {
        return Ok(None);
    }
    let Some(conflict_id) = record.conflict_id else {
        return Ok(None);
    };
    let Some(conflict) = store.get_fact_conflict(conflict_id)? else {
        return Ok(None);
    };
    let loser = match record.verdict {
        DecisionVerdict::CloseOld => conflict.old_fact_id,
        _ => conflict.new_fact_id,
    };
    Ok(store.get_fact(loser)?.map(|f| f.object_norm))
}

/// What a prior decision does to a freshly classified pair — three answers,
/// because two conflated the one that matters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplayOutcome {
    /// The prior decision binds this pair; apply this verdict instead of
    /// asking again.
    Applied(Verdict),
    /// No prior decision applies here; the classifier's verdict stands.
    NotBinding,
    /// A decision exists for this key, and it names a value NEITHER side of
    /// this pair holds — it answered a different question. The pair queues:
    /// applying it in either direction would be a coin toss with an audit
    /// trail.
    Unmatched,
    /// A directional decision with no recorded loser and nothing left to
    /// recover one from. It queues, on EVERY relation [G10]. A decision whose
    /// orientation nobody can prove is not a decision that may close anything:
    /// a caller's ruling applied backwards is corruption whether it concerns a
    /// prescription or an employer.
    Directionless,
}

/// What a prior decision does to a freshly classified pair.
///
/// A CALLER decision is binding in every direction: keep-both suppresses the
/// question, close-old and reject-new re-apply it silently.
///
/// A MACHINE decision is binding ONLY when it is `KeepBoth`, because that
/// outcome closes nothing. Replaying a machine `CloseOld` would re-freeze the
/// exact classifier bug this redesign exists to remove: ~14.5k resolved rows
/// were settled by `facts triage` under the old rules, and those rows must
/// never be able to retire a fact a second time.
///
/// DIRECTION [G3]. `CloseOld` and `RejectNew` name a side of a pair, and the
/// semantic key does not: it sorts the object pair because it identifies the
/// question. So a re-derivation that emits the two values in the opposite
/// order used to map "close the old one" onto the fact the caller had KEPT —
/// the caller decided Acme loses, the store closed Beta. The recorded loser is
/// what makes the verdict travel with the value instead of with the slot.
///
/// `current` is read for one reason: a decision that would change nothing is
/// not replayed, so a settled or absorbed pair does not accumulate
/// `decision.replayed` events describing a no-op.
///
/// The relation's safety class is NOT read [G10]. It used to be: an unprovable
/// orientation queued on a medication and replayed positionally on an employer,
/// on the theory that repeating an old convention beat re-asking about every
/// city a pre-v12 store had settled. That was a scope error. Positional replay
/// is a coin toss, and losing it silently retires whichever value the caller
/// KEPT — which is corruption of the ledger this whole subsystem exists to be.
pub fn replay(
    prior: &DecisionRecord,
    current: Verdict,
    new_object_norm: &str,
    old_object_norm: &str,
) -> ReplayOutcome {
    if matches!(
        current.class,
        ConflictClass::Settled | ConflictClass::Duplicate
    ) {
        return ReplayOutcome::NotBinding;
    }
    match (prior.authority, prior.verdict) {
        (_, DecisionVerdict::Void) => ReplayOutcome::NotBinding,
        (_, DecisionVerdict::KeepBoth) => {
            // Keep-both closes nothing, so it has no orientation to get wrong
            // — and replaying it onto a pair that already coexists would only
            // log a no-op.
            if current.class == ConflictClass::Coexist {
                ReplayOutcome::NotBinding
            } else {
                ReplayOutcome::Applied(Verdict::new(ConflictClass::Coexist, "replayed_keep_both"))
            }
        }
        (DecisionAuthority::Machine, _) => ReplayOutcome::NotBinding,
        (DecisionAuthority::Caller, DecisionVerdict::CloseOld | DecisionVerdict::RejectNew) => {
            replay_directional(prior, new_object_norm, old_object_norm)
        }
    }
}

/// The half of [`replay`] that has a side to get wrong.
fn replay_directional(
    prior: &DecisionRecord,
    new_object_norm: &str,
    old_object_norm: &str,
) -> ReplayOutcome {
    let close_old = Verdict::new(ConflictClass::SupersedeOld, "replayed_close_old");
    let reject_new = Verdict::new(ConflictClass::SupersedeNew, "replayed_reject_new");
    let Some(loser) = prior.loser_object_norm.as_deref() else {
        // A row written before schema v12 recorded which value lost, whose
        // queue row is gone too and which the v12 backfill therefore could not
        // orient: nothing left in the store can say which side the caller
        // retired, so replay STOPS [G10].
        //
        // Falling back to pair order is what reversed a caller's ruling on
        // `metformin 500 mg` vs `1000 mg` once a `kb delete` had cascaded the
        // conflict row away — and, on the ordinary half this arm used to keep,
        // Carol's `works_at` too: the re-derivation emitted the pair in the
        // other order and "close the old one" landed on the employer the
        // caller had kept. Asking again is cheap; retiring the kept value is
        // not.
        return ReplayOutcome::Directionless;
    };
    // The verdict is not consulted past this point ON PURPOSE: the loser IS
    // the answer, and "which of these two rows is the old one" is exactly the
    // fact that changed between the two derivations.
    if loser == old_object_norm {
        ReplayOutcome::Applied(close_old)
    } else if loser == new_object_norm {
        ReplayOutcome::Applied(reject_new)
    } else {
        ReplayOutcome::Unmatched
    }
}

// ===========================================================================
// The pure write planner
// ===========================================================================

/// Whether an assertion OPENS an interval or ENDS one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ObservationState {
    #[default]
    Ongoing,
    /// "stopped taking X", "no longer works at Y" — the utterance names the
    /// END of an interval, not a second one.
    Ended,
}

/// ONE incoming assertion, already canonicalized by the applier so the planner
/// does no I/O and no entity resolution.
#[derive(Debug, Clone)]
pub struct Observation {
    /// Pre-minted so a planned conflict can name it before it is inserted.
    pub fact_id: Uuid,
    pub subject: String,
    pub subject_norm: String,
    /// Canonical relation.
    pub relation: String,
    pub spec: RelationSpec,
    pub object: String,
    pub object_norm: String,
    pub member: String,
    pub confidence: f32,
    pub source_memory_id: Uuid,
    pub source_kind: SourceKind,
    pub fact_type: FactType,
    pub diary: String,
    pub kb: String,
    /// From `resolve_valid_from` / `resolve_valid_until`.
    pub interval: Interval,
    pub state: ObservationState,
    pub autonomy: Autonomy,
    /// `ENGRAM_FACT_QUEUE_ALL`, read once by the applier so the planner stays
    /// pure.
    pub queue_everything: bool,
}

impl Observation {
    /// The row this observation would become. Built by the planner so the
    /// classifier sees one shape, and by the applier so it inserts exactly
    /// what was planned.
    pub fn to_fact(&self, created_at: DateTime<Utc>) -> Fact {
        Fact {
            id: self.fact_id,
            source_memory_id: self.source_memory_id,
            subject: self.subject.clone(),
            subject_norm: self.subject_norm.clone(),
            predicate: self.relation.clone(),
            object: self.object.clone(),
            object_norm: self.object_norm.clone(),
            confidence: self.confidence,
            created_at,
            superseded_by: None,
            superseded_at: None,
            diary: self.diary.clone(),
            fact_type: self.fact_type,
            valid_from: Some(self.interval.from),
            // An OPEN row on arrival even when the plan closes it a moment
            // later: the close goes through `close_fact_interval`, so the
            // journal records who ended it rather than the row appearing
            // already dead.
            valid_until: None,
            resolution: None,
            source_kind: self.source_kind,
            confirmations: 0,
            closed_by: None,
            close_reason: None,
            close_authority: None,
            kb: self.kb.clone(),
        }
    }

    fn view<'a>(&self, fact: &'a Fact) -> FactView<'a> {
        FactView {
            fact,
            relation: self.relation.clone(),
            spec: self.spec,
            member: self.member.clone(),
            interval: self.interval,
        }
    }

    /// The instant a cessation names: the stated end, else the moment the
    /// store learned of it.
    fn end_instant(&self) -> DateTime<Utc> {
        self.interval.until.unwrap_or(self.interval.from)
    }
}

/// WHO is entitled to end an interval this observation says ended [G1].
///
/// `ExplicitEnd` means THE SOURCE ITSELF SAID SO, and only a person can be
/// that source: a `--facts-json` triple the caller asserted, or a value they
/// confirmed. The extraction model's `state: "ended"` token arrives in the
/// same field with none of the standing — it is a HINT — and v0.4.0-rc1 read
/// the two as one thing, so a model that hallucinated "ended" over "Ana
/// confirms she is still taking metformin" retired the prescription with
/// `close_authority: explicit_end`.
///
/// An extracted cessation is therefore a `Heuristic`, which `may_close`
/// refuses on a safety relation (the pair becomes a question a person answers)
/// and allows everywhere else — so "Bruno moved out of Lisbon" still closes
/// the interval it names.
pub fn end_authority(source_kind: SourceKind) -> CloseAuthority {
    match source_kind {
        SourceKind::UserStated | SourceKind::UserConfirmed => CloseAuthority::ExplicitEnd,
        _ => CloseAuthority::Heuristic {
            rule: EXTRACTOR_CESSATION,
        },
    }
}

/// The audit tag an ending writes into `facts.resolution`, DERIVED from the
/// authority so the tag can never claim standing the close did not have. The
/// applier used to hard-code `explicit_end` at both close sites.
pub fn end_resolution_tag(authority: &CloseAuthority) -> &'static str {
    match authority {
        CloseAuthority::Heuristic { rule } => rule,
        other => other.as_str(),
    }
}

/// The row the applier will insert, if the plan opens an interval.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedFact {
    pub id: Uuid,
    pub fact_type: FactType,
    pub interval: Interval,
    /// Pre-closed on arrival: an `Ended` observation with nothing open to
    /// close. The row is born, then closed in the same transaction, so the
    /// journal carries the reason rather than a mute row.
    pub close: Option<(CloseReason, CloseAuthority)>,
}

/// One interval to close, with the authority that entitles the close.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedClose {
    pub fact_id: Uuid,
    pub at: DateTime<Utc>,
    pub reason: CloseReason,
    pub authority: CloseAuthority,
    pub closed_by: Option<Uuid>,
    /// The audit tag that lands in `facts.resolution`.
    pub resolution_tag: &'static str,
}

/// A pair the caller must decide, already carrying its semantic key.
#[derive(Debug, Clone, PartialEq)]
pub struct PlannedConflict {
    pub new_fact_id: Uuid,
    pub old_fact_id: Uuid,
    pub detector: &'static str,
    pub score: Option<f32>,
    pub verdict: Verdict,
    pub key: SemanticKey,
    pub suggested: &'static str,
    pub allowed: &'static [&'static str],
    pub member_key: String,
    pub registry_version: &'static str,
}

/// A prior decision applied instead of asking again. The applier appends one
/// `decision.replayed` per entry, in the same transaction as its effect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedReplay {
    pub decision_id: Uuid,
    pub key: SemanticKey,
    pub verdict: DecisionVerdict,
    pub authority: DecisionAuthority,
    pub new_fact_id: Uuid,
    pub old_fact_id: Uuid,
    pub decided_at: DateTime<Utc>,
}

/// Non-fatal reports that reach the envelope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanNote {
    UnreadableValidUntil {
        raw: String,
    },
    ValidUntilBeforeValidFrom {
        raw: String,
    },
    EndWithoutOpenInterval {
        member: String,
    },
    ReplayApplied {
        key: String,
        verdict: DecisionVerdict,
    },
    /// The machine adjudicator gave no verdict (an error, a timeout, a reply
    /// that is not one). `reason` is a word the applier chose, never the reply.
    AdjudicationFailed {
        reason: &'static str,
    },
    /// The machine adjudicator answered `coexist` or `reject_new` on a safety
    /// pair. Neither is its to give: the pair stays queued and the caller's
    /// observation is kept.
    AdjudicationKeptSafety {
        action: &'static str,
    },
    /// Under `--autonomy full` the pairs queued here are not the pairs the
    /// judge was asked about — the store moved between judging (outside the
    /// write lock) and writing — so no verdict applies and they stay queued.
    AdjudicationStale,
    /// Under `--autonomy full` these pairs appeared only after judging —
    /// typically against an earlier fact of the same batch — so no judge was
    /// asked about them and they stay queued.
    AdjudicationNotAsked,
}

impl PlanNote {
    pub fn kind(&self) -> &'static str {
        match self {
            PlanNote::UnreadableValidUntil { .. } => "unreadable_valid_until",
            PlanNote::ValidUntilBeforeValidFrom { .. } => "valid_until_before_valid_from",
            PlanNote::EndWithoutOpenInterval { .. } => "end_without_open_interval",
            PlanNote::ReplayApplied { .. } => "replay_applied",
            PlanNote::AdjudicationFailed { .. } => "adjudication_failed",
            PlanNote::AdjudicationKeptSafety { .. } => "adjudication_kept_safety",
            PlanNote::AdjudicationStale => "adjudication_stale",
            PlanNote::AdjudicationNotAsked => "adjudication_not_asked",
        }
    }

    pub fn detail(&self) -> String {
        match self {
            PlanNote::UnreadableValidUntil { raw } => {
                format!("valid_until '{raw}' could not be read and was dropped")
            }
            PlanNote::ValidUntilBeforeValidFrom { raw } => {
                format!("valid_until '{raw}' precedes valid_from and was dropped")
            }
            PlanNote::EndWithoutOpenInterval { member } => format!(
                "nothing open to close for '{member}'; a closed interval was recorded instead"
            ),
            PlanNote::ReplayApplied { key, verdict } => format!(
                "a prior decision ({}) on {key} was applied instead of asking again",
                verdict.as_str()
            ),
            PlanNote::AdjudicationFailed { reason } => format!(
                "the adjudicator gave no verdict ({reason}); every pair it was asked about stays \
                 queued for the caller"
            ),
            PlanNote::AdjudicationKeptSafety { action } => format!(
                "the adjudicator answered {action} on a safety relation; only the caller may \
                 clear or discard a safety question, so it stays queued and the observation is \
                 kept"
            ),
            PlanNote::AdjudicationStale => {
                "the store changed between adjudication and this write, \
                 so no verdict applies; the pairs stay queued for the caller"
                    .to_string()
            }
            PlanNote::AdjudicationNotAsked => {
                "these pairs arose after adjudication (an earlier fact of this batch, or a \
                 concurrent write), so no adjudicator was asked; they stay queued for the caller"
                    .to_string()
            }
        }
    }

    /// The same JSON shape as a `date_warnings` entry, so an agent parses one
    /// array shape rather than two.
    pub fn to_json(&self, subject: &str, predicate: &str) -> serde_json::Value {
        json!({
            "kind": self.kind(),
            "subject": subject,
            "predicate": predicate,
            "detail": self.detail(),
        })
    }
}

/// Everything one observation implies, decided without touching a store.
#[derive(Debug, Clone, Default)]
pub struct WritePlan {
    /// The interval to open. `None` = the observation was absorbed or was a
    /// cessation that closed something.
    pub insert: Option<PlannedFact>,
    /// Facts to bump a confirmation on.
    pub confirm: Vec<Uuid>,
    /// Intervals to close.
    pub closes: Vec<PlannedClose>,
    /// Pairs the caller must decide.
    pub queue: Vec<PlannedConflict>,
    /// Prior decisions applied instead of asking again.
    pub replays: Vec<PlannedReplay>,
    /// Non-fatal reports for the envelope.
    pub notes: Vec<PlanNote>,
    /// The rule that absorbed the observation, when nothing was opened and
    /// that was a decision rather than a cessation.
    pub absorbed: Option<&'static str>,
}

/// A prior-decision lookup that may fail. It returns `Result` on purpose: a
/// `SqliteBusy` read as "nobody has decided this" re-asks a settled safety
/// question at best and re-closes an interval the caller reinstated at worst.
pub type PriorDecision<'a> =
    dyn Fn(&SemanticKey) -> Result<Option<DecisionRecord>, StorageError> + 'a;

/// Plan one write.
///
/// `siblings` = every fact (open or closed) in (kb, diary) with the same
/// `subject_norm` and canonical relation. `others` = novelty-gate candidates
/// from other relations, already scored.
pub fn plan_write(
    observation: &Observation,
    siblings: &[Fact],
    others: &[(Fact, f32)],
    mode: ClassifyMode,
    prior: &PriorDecision<'_>,
) -> Result<WritePlan, StorageError> {
    let new_fact = observation.to_fact(observation.interval.from);
    let new_view = observation.view(&new_fact);
    let mut plan = WritePlan::default();

    if observation.state == ObservationState::Ended {
        plan_cessation(observation, siblings, &new_view, &mut plan);
        return Ok(plan);
    }

    // An exact re-assertion of an OPEN interval is a confirmation, whatever
    // the relation's safety class: counting a re-assertion closes nothing, and
    // turning "she is still allergic to penicillin" into a question is the
    // storm wearing a different hat.
    if let Some(same) = siblings
        .iter()
        .filter(|f| f.id != observation.fact_id)
        .find(|f| f.object_norm == observation.object_norm && is_open_fact(f))
    {
        plan.confirm.push(same.id);
        plan.absorbed = Some("duplicate_object");
        return Ok(plan);
    }

    plan.insert = Some(PlannedFact {
        id: observation.fact_id,
        fact_type: observation.fact_type,
        interval: observation.interval,
        // "Elias lived in London FROM 2015 UNTIL 2020" opens an interval that
        // is already closed. The row is born, then closed, so the journal
        // records an end the text stated rather than a row that arrives mute
        // and dead.
        close: observation
            .interval
            .until
            .map(|_| (CloseReason::Stopped, end_authority(observation.source_kind))),
    });

    for old in siblings.iter().filter(|f| f.id != observation.fact_id) {
        let old_view = FactView::of(old);
        let verdict = classify_views(&new_view, &old_view, None, mode);
        dispatch(
            observation,
            &new_view,
            &old_view,
            verdict,
            "slot",
            None,
            prior,
            &mut plan,
        )?;
        if plan.absorbed.is_some() {
            return Ok(plan);
        }
    }
    for (old, score) in others.iter().filter(|(f, _)| f.id != observation.fact_id) {
        let old_view = FactView::of(old);
        let verdict = classify_views(&new_view, &old_view, Some(*score), mode);
        dispatch(
            observation,
            &new_view,
            &old_view,
            verdict,
            "semantic_cosine",
            Some(*score),
            prior,
            &mut plan,
        )?;
        if plan.absorbed.is_some() {
            return Ok(plan);
        }
    }
    Ok(plan)
}

/// "stopped taking X" closes every open interval whose member matches — IF the
/// ending carries authority to close them. If nothing is open, a pre-closed
/// interval is recorded and the caller is told: the utterance does claim the
/// person once took X, and a row the store can show you beats a statement it
/// silently dropped.
///
/// The gate is the whole reason this is not a two-line loop. `end_authority`
/// says who is speaking, and a `Heuristic` ending — an extraction model's
/// `state: "ended"` — is REFUSED on a safety relation. A refusal is not a
/// silent skip: the claim is still recorded as a point interval, and the pair
/// becomes the question a person answers.
fn plan_cessation(
    observation: &Observation,
    siblings: &[Fact],
    new_view: &FactView<'_>,
    plan: &mut WritePlan,
) {
    let end_at = observation.end_instant();
    let authority = end_authority(observation.source_kind);
    let tag = end_resolution_tag(&authority);
    let mut closed_any = false;
    let mut refused: Vec<&Fact> = Vec::new();
    for old in siblings.iter().filter(|f| f.id != observation.fact_id) {
        if !is_open_fact(old) {
            continue;
        }
        let old_view = FactView::of(old);
        if old_view.member != observation.member {
            continue;
        }
        // THE ONE FUNNEL. A person's explicit end is authorizing evidence on
        // every relation; a model's is not, and the gate is where that stops
        // being a comment.
        if may_close(&old_view.spec, &authority) != CloseVerdict::Allow {
            refused.push(old);
            continue;
        }
        plan.closes.push(PlannedClose {
            fact_id: old.id,
            at: close_at(&old_view.interval, end_at),
            reason: CloseReason::Stopped,
            authority: authority.clone(),
            closed_by: None,
            resolution_tag: tag,
        });
        closed_any = true;
    }
    if !closed_any {
        // The utterance gives an END and no beginning, so the honest interval
        // is a POINT at the stated end: the store knows the state held up to
        // then and nothing about when it started. Dating it from the memory
        // instead would clamp the close forward to "now" and record a
        // medication that ended today.
        let from = observation.interval.from.min(end_at);
        plan.insert = Some(PlannedFact {
            id: observation.fact_id,
            fact_type: observation.fact_type,
            interval: Interval {
                from,
                until: Some(end_at),
            },
            close: Some((CloseReason::Stopped, authority.clone())),
        });
    }
    if refused.is_empty() {
        if !closed_any {
            plan.notes.push(PlanNote::EndWithoutOpenInterval {
                member: observation.member.clone(),
            });
        }
        return;
    }
    // Something IS open — the ending simply had no standing to close it, so
    // `end_without_open_interval` would be a lie. The caller gets the pair
    // instead, and `suggested_action` answers `requires_context` because only
    // a safety relation can reach this line.
    for old in refused {
        let old_view = FactView::of(old);
        let key = SemanticKey::for_pair(new_view, &old_view);
        plan.queue.push(planned_conflict(
            observation,
            new_view,
            &old_view,
            Verdict::escalated(
                ConflictClass::Queue,
                EXTRACTOR_CESSATION,
                ConflictClass::SupersedeOld,
            ),
            "cessation",
            None,
            key,
        ));
    }
}

/// One classified pair becomes a close, a confirmation, a question, or
/// nothing — after a prior decision has had its say.
#[allow(clippy::too_many_arguments)]
fn dispatch(
    observation: &Observation,
    new_view: &FactView<'_>,
    old_view: &FactView<'_>,
    verdict: Verdict,
    detector: &'static str,
    score: Option<f32>,
    prior: &PriorDecision<'_>,
    plan: &mut WritePlan,
) -> Result<(), StorageError> {
    let key = SemanticKey::for_pair(new_view, old_view);
    // Replay runs BEFORE a conflict row is opened. A failed lookup aborts the
    // write rather than degrading to "nobody has decided this".
    let (verdict, replayed) = match prior(&key)? {
        Some(record) => match replay(
            &record,
            verdict,
            &new_view.fact.object_norm,
            &old_view.fact.object_norm,
        ) {
            ReplayOutcome::Applied(applied) => {
                plan.replays.push(PlannedReplay {
                    decision_id: record.id,
                    key: key.clone(),
                    verdict: record.verdict,
                    authority: record.authority,
                    new_fact_id: observation.fact_id,
                    old_fact_id: old_view.fact.id,
                    decided_at: record.decided_at,
                });
                plan.notes.push(PlanNote::ReplayApplied {
                    key: key.encode(),
                    verdict: record.verdict,
                });
                (applied, Some(record.decided_at))
            }
            ReplayOutcome::NotBinding => (verdict, None),
            // No `PlannedReplay` and no `replay_applied` note: nothing was
            // replayed. The classifier's own close is withdrawn too — a stored
            // answer we cannot map is a reason to ask, not to act.
            ReplayOutcome::Unmatched => (
                Verdict::new(ConflictClass::Queue, "replay_loser_unmatched"),
                None,
            ),
            // Same withdrawal, different reason: the stored answer may well be
            // about this pair, but nothing can say which half of it lost. On
            // any relation [G10] — a wrong guess retires a medication here and
            // an employer there, and both are the caller's ruling inverted.
            ReplayOutcome::Directionless => (
                Verdict::new(ConflictClass::Queue, "replay_orientation_unknown"),
                None,
            ),
        },
        None => (verdict, None),
    };

    // `ENGRAM_FACT_QUEUE_ALL` is the test seam that rebuilds the pre-triage
    // write path — the one that turned 2,953 imported facts into 9,510 queued
    // "conflicts" — so `facts triage`'s tests can construct the backlog it
    // exists to clean up. It keeps its exact present-day reach: EVERY
    // classification queues except a duplicate, because a confirmation is not
    // a decision. The escalation carries no `escalated_from`: a test seam must
    // not change the advice a caller is given.
    if observation.queue_everything && verdict.class != ConflictClass::Duplicate {
        plan.queue.push(planned_conflict(
            observation,
            new_view,
            old_view,
            Verdict::new(ConflictClass::Queue, verdict.rule),
            detector,
            score,
            key,
        ));
        return Ok(());
    }

    match verdict.class {
        ConflictClass::Coexist | ConflictClass::Settled => {
            if verdict.rule == "restates_closed_interval" {
                // Re-opening a window someone deliberately ended is the one
                // thing this arm exists to prevent.
                plan.absorb("restates_closed_interval");
            }
        }
        ConflictClass::Duplicate => {
            plan.absorb("duplicate_object");
            plan.confirm.push(old_view.fact.id);
        }
        ConflictClass::SupersedeOld => {
            let authority = close_authority(&verdict, replayed);
            let at = close_at(&old_view.interval, observation.interval.from);
            push_close(
                observation,
                PlannedClose {
                    fact_id: old_view.fact.id,
                    at,
                    reason: close_reason(&verdict),
                    authority,
                    closed_by: Some(observation.fact_id),
                    resolution_tag: verdict.rule,
                },
                &old_view.spec,
                (new_view, old_view, verdict, detector, score, key),
                plan,
            );
        }
        ConflictClass::SupersedeNew => {
            let authority = close_authority(&verdict, replayed);
            let at = close_at(&observation.interval, old_view.interval.from);
            push_close(
                observation,
                PlannedClose {
                    fact_id: observation.fact_id,
                    at,
                    reason: close_reason(&verdict),
                    authority,
                    closed_by: Some(old_view.fact.id),
                    resolution_tag: verdict.rule,
                },
                &new_view.spec,
                (new_view, old_view, verdict, detector, score, key),
                plan,
            );
        }
        ConflictClass::Queue => {
            plan.queue.push(planned_conflict(
                observation,
                new_view,
                old_view,
                verdict,
                detector,
                score,
                key,
            ));
        }
    }
    Ok(())
}

impl WritePlan {
    /// The observation turned out to be news to nobody, so NOTHING it implied
    /// survives.
    ///
    /// Clearing the rest of the plan is not tidiness. An earlier sibling can
    /// already have planned a close — "Ana lives in Paris" arrives, closes the
    /// open London interval, and is then absorbed by a Paris interval that was
    /// deliberately ended in 2020. Keeping the close would retire the current
    /// value on the strength of an assertion the planner just discarded.
    ///
    /// Public because the planner is not the only thing that discards an
    /// observation: a machine adjudicator's `reject_new` used to hand-roll
    /// half of this, kept the closes, and retired a birthday "on behalf of" a
    /// fact that was never written — a `closed_by` pointing at no row.
    pub fn absorb(&mut self, rule: &'static str) {
        self.insert = None;
        self.confirm.clear();
        self.closes.clear();
        self.queue.clear();
        self.replays.clear();
        // A withdrawn replay must not be reported as applied.
        self.notes
            .retain(|n| !matches!(n, PlanNote::ReplayApplied { .. }));
        self.absorbed = Some(rule);
    }
}

type PairContext<'a, 'f> = (
    &'a FactView<'f>,
    &'a FactView<'f>,
    Verdict,
    &'static str,
    Option<f32>,
    SemanticKey,
);

/// Step 9.5 — the two autonomy policies, applied where the close is decided
/// rather than in the applier. Leaving them downstream let a plan assert a
/// close the applier then refused, which is two policies in two places.
fn push_close(
    observation: &Observation,
    close: PlannedClose,
    spec: &RelationSpec,
    pair: PairContext<'_, '_>,
    plan: &mut WritePlan,
) {
    let (new_view, old_view, verdict, detector, score, key) = pair;
    // Defence in depth. `safety_gate` already downgraded every safety close,
    // so a refusal here means a rule reached a close it was never entitled to;
    // it becomes a question rather than a silent write.
    if let CloseVerdict::Refuse { .. } = may_close(spec, &close.authority) {
        plan.queue.push(planned_conflict(
            observation,
            new_view,
            old_view,
            Verdict::escalated(ConflictClass::Queue, "close_refused", verdict.class),
            detector,
            score,
            key,
        ));
        return;
    }
    // `strict` promised never to auto-supersede. It did NOT promise to
    // disbelieve the text or re-ask a question the caller already answered, so
    // an explicit end and a replayed caller decision are not escalated.
    let machine_close = matches!(
        close.authority,
        CloseAuthority::SuccessorInterval | CloseAuthority::Heuristic { .. }
    );
    if observation.autonomy == Autonomy::Strict && machine_close {
        plan.queue.push(planned_conflict(
            observation,
            new_view,
            old_view,
            Verdict::escalated(ConflictClass::Queue, verdict.rule, verdict.class),
            detector,
            score,
            key,
        ));
        return;
    }
    plan.closes.push(close);
}

fn planned_conflict(
    observation: &Observation,
    new_view: &FactView<'_>,
    old_view: &FactView<'_>,
    verdict: Verdict,
    detector: &'static str,
    score: Option<f32>,
    key: SemanticKey,
) -> PlannedConflict {
    PlannedConflict {
        new_fact_id: observation.fact_id,
        old_fact_id: old_view.fact.id,
        detector,
        score,
        verdict,
        key,
        suggested: suggested_action(&verdict, new_view, old_view),
        allowed: allowed_actions(&verdict, new_view, old_view),
        member_key: observation.member.clone(),
        registry_version: REGISTRY_VERSION,
    }
}

/// WHO is entitled to this close. The gate does not care how confident a
/// heuristic was, so the mapping is by rule, not by score.
fn close_authority(verdict: &Verdict, replayed_at: Option<DateTime<Utc>>) -> CloseAuthority {
    if let Some(decided_at) = replayed_at {
        return CloseAuthority::ReplayedCaller { decided_at };
    }
    match verdict.rule {
        // A person stating their own value IS the evidence; an immutable
        // relation refuses a successor, and this is not one.
        "correction_by_user" => CloseAuthority::Caller {
            actor: "user_stated".to_string(),
        },
        "slot_lww"
        | "exact_match"
        | "recency_supersede"
        | "recency_stale"
        | "member_attribute_change" => CloseAuthority::SuccessorInterval,
        rule => CloseAuthority::Heuristic { rule },
    }
}

fn close_reason(verdict: &Verdict) -> CloseReason {
    match verdict.rule {
        "correction_by_user" => CloseReason::Corrected,
        _ => CloseReason::Superseded,
    }
}

// ===========================================================================
// The backlog drain (spec §4.1 Phase 2)
// ===========================================================================

/// What `facts triage` should do with ONE already-queued row, under the
/// interval rules.
///
/// Exported as a pure function so the drain has one implementation rather than
/// an inline copy in `commands/facts.rs` that can drift from the write path —
/// the drift that produced two safety defences with two different answers.
/// `settled` names facts an earlier row in the same sweep already closed.
pub fn triage_verdict(
    new: &Fact,
    old: &Fact,
    score: Option<f32>,
    settled: &dyn Fn(Uuid) -> bool,
) -> Verdict {
    let verdict = classify_pair(new, old, score, ClassifyMode::Triage);
    // An earlier row in this run already closed one of the two intervals, so
    // this pair has nothing left to decide — the snapshot it was classified
    // from predates that write. A coexistence verdict keeps its more
    // informative rule name: it is resolved either way and touches no fact.
    if verdict.class != ConflictClass::Coexist && (settled(new.id) || settled(old.id)) {
        return Verdict::new(ConflictClass::Settled, "already_settled");
    }
    verdict
}

/// The audit tag a triage resolution writes into `facts.resolution`.
pub fn triage_resolution(verdict: &Verdict) -> &'static str {
    match verdict.class {
        ConflictClass::Duplicate => "triage_duplicate",
        ConflictClass::SupersedeOld => "triage_recency",
        ConflictClass::SupersedeNew => "triage_stale",
        _ => verdict.rule,
    }
}

/// The decision a triage resolution records, so the sweep is replayable.
/// Every one of them is `authority = machine`, and per [`replay`] only its
/// `keep_both` verdicts bind a later write.
pub fn triage_decision(verdict: &Verdict) -> DecisionVerdict {
    match verdict.class {
        ConflictClass::SupersedeOld => DecisionVerdict::CloseOld,
        ConflictClass::SupersedeNew | ConflictClass::Duplicate => DecisionVerdict::RejectNew,
        ConflictClass::Coexist => DecisionVerdict::KeepBoth,
        _ => DecisionVerdict::Void,
    }
}

/// The member-identity heuristic's version, for the queue rows this lane
/// writes. Re-exported so a caller needs one import, not two.
pub const MEMBER_VERSION: &str = MEMBER_IDENTITY_VERSION;

#[cfg(test)]
mod tests;

//! `engram state <subject>` — the compiled current-state view.
//!
//! WHY THIS COMMAND EXISTS. `engram facts show ada` returns a flat list in
//! which a medication she STOPPED and a medication she takes today look
//! identical, because both rows have `superseded_by IS NULL` — closing an
//! interval with reason `stopped` leaves no successor. An agent reading that
//! list has no way to tell what is true now, which is the only question a
//! caller ever actually asks. `state` answers it from ONE source: the open
//! intervals, grouped by relation, each with the provenance chain that
//! produced it, the confidence and the basis of that confidence, and the open
//! questions still queued about the subject.
//!
//! Two deliberate absences:
//!
//! - **No retrieval.** `state` runs no vector search and scores no chunks. A
//!   current-state answer that depends on a k-NN cutoff is not a current-state
//!   answer, which is also why `metadata.scope.chunks_considered` is omitted
//!   rather than reported as 0 and why `absence.searched_chunks` is 0.
//! - **No new rule.** Interval semantics, member identity and the relation
//!   registry are `engram_core`'s (R1). This file consumes them; it never
//!   re-derives one. The single predicate it does own is `contains`, because
//!   `Interval` ships no containment method and this lane does not add methods
//!   to another lane's file.

use std::collections::{BTreeMap, HashMap, HashSet};

use chrono::{DateTime, Utc};
use engram_core::handle::HandleKind;
use engram_core::interval::Interval;
use engram_core::relations::{
    canonical_relation, is_safety_slot, member_identity, relation_spec, Arity, Mutability,
    RelationSpec, Temporality, ValueDomain,
};
use engram_core::types::Fact;
use serde_json::{json, Map, Value};
use uuid::Uuid;

use crate::context::AppContext;
use crate::error::CliError;
use crate::output::{print_success, Absence, Listing, Metadata, NearestSubject, Scope, Verdict};
use crate::read_surface::{self, CursorKey, Provenance};

/// The confidence terms, pinned as constants so the number and its published
/// basis can never drift apart (spec §2.3).
///
/// These are CHOSEN, not measured: no dataset in this repo says a
/// `user_stated` fact is five points better than an extracted one. The design
/// decision is to make the number auditable — `confidence_basis` sums to
/// `confidence`, and the tests pin every term — so it can be recalibrated
/// later without changing a caller.
const CONF_USER_STATED: f32 = 0.05;
const CONF_COMPILE_LLM: f32 = -0.05;
const CONF_IMPORT: f32 = -0.02;
const CONF_PER_CONFIRMATION: f32 = 0.02;
const CONF_CONFIRMATION_CAP: f32 = 0.06;
const CONF_STALENESS_PER_YEAR: f32 = -0.02;
const CONF_STALENESS_CAP: f32 = -0.10;
const CONF_CONTESTED: f32 = -0.15;
/// Never 1.0. An extracted belief is never certain, and a caller who reads
/// 1.0 stops checking.
const CONF_MIN: f32 = 0.01;
const CONF_MAX: f32 = 0.99;

/// The subject-resolution bar, the same Jaro-Winkler threshold the write path
/// uses (`fact_write::canonicalize_subject`). Mirrored, not shared: that
/// function takes `&str` scopes and returns no verdict, and `resolved_from` is
/// exactly what tells a caller "I answered about a different person".
const FUZZY_THRESHOLD: f64 = 0.92;

/// How many open conflicts are examined for the `contested` term before the
/// field is reported as unknown. A boolean that means "we did not look" is the
/// count-meaning-page defect in miniature.
const CONTESTED_CAP: usize = 500;

const SNIPPET_CHARS: usize = 240;

pub struct StateArgs {
    pub subject: String,
    /// `"*"` = every KB.
    pub kb: String,
    /// `"*"` = every diary.
    pub diary: String,
    pub relation: Option<String>,
    pub as_of: Option<String>,
    pub include_closed: bool,
    pub limit: usize,
    pub cursor: Option<String>,
}

/// Which branch answered — and therefore whether the caller was answered
/// about the name they typed.
#[derive(Debug, Clone, PartialEq)]
pub enum SubjectResolution {
    Exact,
    Alias,
    Fuzzy { score: f64 },
    Raw,
}

impl SubjectResolution {
    pub fn as_str(&self) -> &'static str {
        match self {
            SubjectResolution::Exact => "exact",
            SubjectResolution::Alias => "alias",
            SubjectResolution::Fuzzy { .. } => "fuzzy",
            SubjectResolution::Raw => "raw",
        }
    }
}

/// Every term that went into a `confidence`, published beside it.
#[derive(Debug, Clone, PartialEq)]
pub struct ConfidenceBasis {
    pub extraction: f32,
    pub source_kind: f32,
    pub confirmations: f32,
    pub staleness: f32,
    /// `None` means WE DID NOT LOOK (the conflict cap was hit), and serializes
    /// as JSON null. `Some(0.0)` means we looked and it is not contested.
    pub contested: Option<f32>,
}

impl ConfidenceBasis {
    fn to_json(&self) -> Value {
        json!({
            "extraction": self.extraction,
            "source_kind": self.source_kind,
            "confirmations": self.confirmations,
            "staleness": self.staleness,
            "contested": self.contested,
        })
    }
}

/// The sum of the published terms, clamped. Anything else printed as
/// `confidence` would make `confidence_basis` decoration.
pub fn state_confidence(basis: &ConfidenceBasis) -> f32 {
    let sum = basis.extraction
        + basis.source_kind
        + basis.confirmations
        + basis.staleness
        + basis.contested.unwrap_or(0.0);
    sum.clamp(CONF_MIN, CONF_MAX)
}

pub struct StateValue {
    pub fact: Fact,
    pub member: String,
    pub confidence: f32,
    pub basis: ConfidenceBasis,
    pub provenance: Provenance,
    /// Closed intervals for the same member, only under `--include-closed`.
    pub history: Vec<Fact>,
}

pub struct RelationGroup {
    pub relation: String,
    pub spec: RelationSpec,
    /// How many open values this relation holds at `as_of` — counted over the
    /// WHOLE current set, never over the page. Under `--limit` the paged
    /// `values` may be a subset (or empty); this number never shrinks with
    /// them, because "open_count: 1" over a store holding two open medications
    /// is the envelope lying about a safety slot.
    pub open_count: usize,
    pub values: Vec<StateValue>,
    pub open_questions: Vec<Value>,
    /// Closed intervals of this relation that no open value inherited, under
    /// `--include-closed`. A stopped medication has no successor to hang from.
    pub history: Vec<Fact>,
}

pub struct StateView {
    pub subject: String,
    pub subject_norm: String,
    pub resolved_from: SubjectResolution,
    pub as_of: DateTime<Utc>,
    pub groups: Vec<RelationGroup>,
    pub open_questions: Vec<Value>,
    pub returned: usize,
    pub total: i64,
    pub cursor: Option<String>,
    pub scanned_kbs: Vec<String>,
    pub diary_label: String,
    pub nearest_subjects: Vec<NearestSubject>,
    pub absence: Option<Absence>,
}

/// Half-open `[from, until)` — EXCEPT for point intervals.
///
/// M14: the first draft returned `iv.from == t` for a point, which makes every
/// `Instant` relation vanish from every `--as-of` query except one asked at the
/// exact microsecond of the event. A birthday is `Instant` + `Immutable`: it is
/// true from its instant onward, forever, so `state ada --as-of 2026-01-01`
/// must still report it.
fn contains(iv: &Interval, t: DateTime<Utc>) -> bool {
    if iv.is_point() {
        return iv.from <= t;
    }
    iv.from <= t && iv.until.is_none_or(|u| u > t)
}

/// Which SLOT an interval belonged to. On a single-valued relation there is
/// one slot and the relation names it; on a set-valued one each member is its
/// own slot, so a stopped metformin never attaches to a current empagliflozin.
fn history_key(f: &Fact) -> String {
    let canonical = canonical_relation(&f.predicate);
    if relation_spec(&canonical).is_single_valued() {
        canonical
    } else {
        format!(
            "{canonical}\u{1f}{}",
            member_identity(&canonical, &f.object_norm)
        )
    }
}

fn history_json(f: &Fact) -> Value {
    json!({
        "handle": read_surface::handle(HandleKind::Fact, f.id),
        "id": f.id.to_string(),
        "object": f.object,
        "valid_from": f.valid_from.map(|t| t.to_rfc3339()),
        "valid_until": f.valid_until.map(|t| t.to_rfc3339()),
        "close_reason": f.close_reason,
        "close_authority": f.close_authority,
    })
}

fn normalize(s: &str) -> String {
    s.trim().to_lowercase()
}

fn token_sort(s: &str) -> String {
    let mut tokens: Vec<&str> = s
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .collect();
    tokens.sort_unstable();
    tokens.join(" ")
}

fn snippet(content: &str) -> String {
    let flat = content.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= SNIPPET_CHARS {
        return flat;
    }
    let mut out: String = flat.chars().take(SNIPPET_CHARS).collect();
    out.push('…');
    out
}

fn arity_class(spec: &RelationSpec) -> &'static str {
    match spec.arity {
        Arity::SetValued { .. } => "set_valued",
        Arity::SingleValued { .. } => "single_valued",
    }
}

fn mutability_str(spec: &RelationSpec) -> &'static str {
    match spec.mutability {
        Mutability::Mutable => "mutable",
        Mutability::Immutable => "immutable",
    }
}

fn temporality_str(spec: &RelationSpec) -> &'static str {
    match spec.temporality {
        Temporality::State => "state",
        Temporality::Instant => "instant",
    }
}

fn value_domain_str(spec: &RelationSpec) -> &'static str {
    match spec.value {
        ValueDomain::Date => "date",
        ValueDomain::Text => "text",
    }
}

/// `"*"` is a read-side wildcard, never a name. Passing the literal into an
/// equality-filtered query searches a KB called `*`, finds nothing, and
/// degrades every default invocation to "unknown subject" — the default
/// invocation being the only broken one is exactly how a bug survives review.
fn scope_filter(value: &str) -> Option<&str> {
    if value.trim() == "*" || value.trim().is_empty() {
        None
    } else {
        Some(value.trim())
    }
}

fn parse_as_of(raw: &Option<String>) -> Result<Option<DateTime<Utc>>, CliError> {
    let Some(raw) = raw else { return Ok(None) };
    let raw = raw.trim();
    if let Ok(dt) = DateTime::parse_from_rfc3339(raw) {
        return Ok(Some(dt.with_timezone(&Utc)));
    }
    // A bare date means the START of that UTC day, matching `recall --since`.
    if let Ok(d) = chrono::NaiveDate::parse_from_str(raw, "%Y-%m-%d") {
        return Ok(Some(DateTime::from_naive_utc_and_offset(
            d.and_hms_opt(0, 0, 0).expect("midnight exists"),
            Utc,
        )));
    }
    Err(CliError::BadInput(format!(
        "--as-of `{raw}` is not a timestamp — pass RFC3339 (2026-03-01T00:00:00Z) or a bare date \
         (2026-03-01)"
    )))
}

/// The four branches of `fact_write::canonicalize_subject`, in the same order
/// and at the same threshold, but over the EXPANDED scope and REPORTING which
/// branch matched.
fn resolve_subject(
    ctx: &AppContext,
    diary: Option<&str>,
    kb: Option<&str>,
    raw: &str,
) -> Result<(String, String, SubjectResolution), CliError> {
    let raw_norm = normalize(raw);
    if raw_norm.is_empty() {
        return Ok((raw.to_string(), raw_norm, SubjectResolution::Raw));
    }
    if !ctx
        .store
        .list_facts_by_subject_scoped(&raw_norm, diary, kb, true)?
        .is_empty()
    {
        return Ok((raw.to_string(), raw_norm, SubjectResolution::Exact));
    }
    // Aliases are KB-scoped in storage, so an unscoped read walks the KBs in
    // `list_kbs()` order and takes the first hit — deterministic, and the same
    // order two runs on one store will always produce.
    let kbs: Vec<String> = match kb {
        Some(one) => vec![one.to_string()],
        None => ctx
            .store
            .list_kbs()?
            .into_iter()
            .map(|k| k.name)
            .collect::<Vec<_>>(),
    };
    for name in &kbs {
        if let Some(canonical) = ctx.store.resolve_entity_alias(name, &raw_norm)? {
            let canon_norm = normalize(&canonical);
            if !canon_norm.is_empty() {
                return Ok((canonical, canon_norm, SubjectResolution::Alias));
            }
        }
    }
    let target = token_sort(&raw_norm);
    let mut best: Option<(f64, String, String)> = None;
    for (display, norm) in ctx.store.distinct_fact_subjects(diary, kb)? {
        let score = strsim::jaro_winkler(&target, &token_sort(&norm));
        if score >= FUZZY_THRESHOLD && best.as_ref().map(|(b, _, _)| score > *b).unwrap_or(true) {
            best = Some((score, display, norm));
        }
    }
    if let Some((score, display, norm)) = best {
        return Ok((display, norm, SubjectResolution::Fuzzy { score }));
    }
    Ok((raw.to_string(), raw_norm, SubjectResolution::Raw))
}

/// The near-miss list that makes an absence certificate actionable.
///
/// KNOWN BIAS, stated rather than hidden: `distinct_fact_subjects` filters on
/// the open-interval predicate, so a subject whose every interval is closed
/// can be missing here. Tolerable for a SPELLING HINT and only there — the
/// state answer itself comes from `open_intervals`, never from this list.
fn nearest_subjects(
    ctx: &AppContext,
    diary: Option<&str>,
    kb: Option<&str>,
    raw_norm: &str,
    n: usize,
) -> Result<Vec<NearestSubject>, CliError> {
    let target = token_sort(raw_norm);
    let mut scored: Vec<NearestSubject> = ctx
        .store
        .distinct_fact_subjects(diary, kb)?
        .into_iter()
        .map(|(display, norm)| NearestSubject {
            subject: display,
            score: strsim::jaro_winkler(&target, &token_sort(&norm)),
        })
        .collect();
    scored.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.subject.cmp(&b.subject))
    });
    scored.truncate(n);
    Ok(scored)
}

fn confidence_for(
    f: &Fact,
    spec: &RelationSpec,
    contested: Option<bool>,
    as_of: DateTime<Utc>,
) -> ConfidenceBasis {
    let source_kind = match f.source_kind.as_str() {
        "user_stated" | "user_confirmed" => CONF_USER_STATED,
        "compile_llm" => CONF_COMPILE_LLM,
        "import" => CONF_IMPORT,
        _ => 0.0,
    };
    let confirmations = (f.confirmations as f32 * CONF_PER_CONFIRMATION).min(CONF_CONFIRMATION_CAP);
    // An immutable slot cannot go stale: a blood type learned in 2019 is as
    // true today as it was then.
    let staleness = if spec.mutability == Mutability::Immutable {
        0.0
    } else {
        // `valid_from` is optional, so staleness measures from the observation
        // when the event time is unknown — a fact with no event time is as
        // stale as its write, not infinitely fresh.
        let since = f.valid_from.unwrap_or(f.created_at);
        let years = (as_of - since).num_days() as f32 / 365.0;
        if years <= 0.0 {
            0.0
        } else {
            (years.floor() * CONF_STALENESS_PER_YEAR).max(CONF_STALENESS_CAP)
        }
    };
    ConfidenceBasis {
        extraction: f.confidence,
        source_kind,
        confirmations,
        staleness,
        contested: contested.map(|c| if c { CONF_CONTESTED } else { 0.0 }),
    }
}

fn provenance_for(ctx: &AppContext, f: &Fact) -> Result<Provenance, CliError> {
    let chunks = ctx.store.chunks_for_memory(f.source_memory_id)?;
    let first = chunks.first();
    let document_id = ctx.store.document_for_memory(f.source_memory_id)?;
    let observed_at = ctx
        .store
        .memory_times(f.source_memory_id)?
        .map(|(event_time, created_at)| event_time.unwrap_or(created_at));
    Ok(Provenance {
        chunk_id: first.map(|c| c.chunk_id),
        memory_id: Some(f.source_memory_id),
        document_id,
        kb: Some(f.kb.clone()),
        diary: Some(f.diary.clone()),
        derived_from: Some(read_surface::DERIVED_FACT),
        observed_at,
        snippet: first.map(|c| snippet(&c.content)),
    })
}

/// D12's struct with the mutator handles attached. `observed_at` and `snippet`
/// used to be re-inserted here because `Provenance::to_json` dropped them;
/// it now serializes every field of the struct, so this adds only the handles.
fn provenance_json(p: &Provenance) -> Value {
    let mut obj = match p.to_json() {
        Value::Object(m) => m,
        other => return other,
    };
    // The handle a mutator accepts, beside the raw id: a chunk id that `edit`
    // rejects is the exact defect typed handles exist to remove.
    if let Some(m) = p.memory_id {
        obj.insert(
            "memory".to_string(),
            json!(read_surface::handle(HandleKind::Memory, m)),
        );
    }
    if let Some(c) = p.chunk_id {
        obj.insert(
            "chunk".to_string(),
            json!(read_surface::handle(HandleKind::Chunk, c)),
        );
    }
    if let Some(d) = p.document_id {
        obj.insert(
            "document".to_string(),
            json!(read_surface::handle(HandleKind::Document, d)),
        );
    }
    Value::Object(obj)
}

/// The open questions still queued about THIS subject.
///
/// The storage filter is a LIKE, not an equality: `list_open_fact_conflicts_
/// with_facts` builds `%<subject>%`, so `state ada` would inherit Adam
/// Example's conflicts. Re-filtering on the exact `subject_norm` is not
/// belt-and-braces — a state view that reports another person's open question
/// is worse than one that reports none.
fn open_questions_for(
    ctx: &AppContext,
    subject_norm: &str,
    diary: Option<&str>,
    kb: Option<&str>,
) -> Result<(Vec<Value>, HashSet<Uuid>, bool), CliError> {
    let rows = ctx.store.list_open_fact_conflicts_with_facts(
        diary,
        kb,
        Some(subject_norm),
        CONTESTED_CAP,
        0,
    )?;
    let capped = rows.len() >= CONTESTED_CAP;
    let mut contested: HashSet<Uuid> = HashSet::new();
    let mut out: Vec<Value> = Vec::new();
    for (row, new_fact, old_fact) in rows {
        if new_fact.subject_norm != subject_norm && old_fact.subject_norm != subject_norm {
            continue;
        }
        contested.insert(new_fact.id);
        contested.insert(old_fact.id);
        let canonical = canonical_relation(&new_fact.predicate);
        let safety = is_safety_slot(&canonical);
        let suggested = read_surface::suggested_action_for(row.suggested.as_deref(), safety);
        let allowed: &'static [&'static str] = &["keep-both", "supersede-old", "reject-new"];
        let mut entry = Map::new();
        entry.insert(
            "handle".to_string(),
            json!(read_surface::handle(HandleKind::Conflict, row.id)),
        );
        entry.insert("id".to_string(), json!(row.id.to_string()));
        entry.insert("relation".to_string(), json!(canonical));
        entry.insert("safety".to_string(), json!(safety));
        entry.insert(
            "new".to_string(),
            json!({ "handle": read_surface::handle(HandleKind::Fact, new_fact.id),
                    "id": new_fact.id.to_string(), "object": new_fact.object }),
        );
        entry.insert(
            "old".to_string(),
            json!({ "handle": read_surface::handle(HandleKind::Fact, old_fact.id),
                    "id": old_fact.id.to_string(), "object": old_fact.object }),
        );
        entry.insert("detector".to_string(), json!(row.detector));
        entry.insert("suggested_action".to_string(), json!(suggested));
        crate::respond::attach(
            &mut entry,
            &crate::respond::resolve_conflict(row.id, allowed),
        );
        out.push(Value::Object(entry));
    }
    Ok((out, contested, capped))
}

/// The pipeline. Split from `run` so the daemon (wave 6) and the tests can
/// build the payload without printing.
pub fn compile_state(ctx: &AppContext, args: &StateArgs) -> Result<StateView, CliError> {
    let kb_filter = scope_filter(&args.kb);
    let diary_filter = scope_filter(&args.diary);
    let as_of = parse_as_of(&args.as_of)?.unwrap_or_else(Utc::now);
    let relation = args
        .relation
        .as_deref()
        .map(|r| canonical_relation(r.trim()));

    let (display, subject_norm, resolved_from) =
        resolve_subject(ctx, diary_filter, kb_filter, &args.subject)?;

    let scanned_kbs: Vec<String> = match kb_filter {
        Some(one) => vec![one.to_string()],
        None => ctx.store.list_kbs()?.into_iter().map(|k| k.name).collect(),
    };

    // `--as-of` in the past and `--include-closed` both need the closed rows;
    // otherwise "current" is the only thing read, from the only source of it.
    let need_closed = args.include_closed || args.as_of.is_some();
    let facts = ctx.store.open_intervals(
        kb_filter,
        diary_filter,
        &subject_norm,
        relation.as_deref(),
        need_closed,
    )?;

    let (questions, contested_ids, contested_capped) =
        open_questions_for(ctx, &subject_norm, diary_filter, kb_filter)?;

    // Split into what holds at `as_of` and what does not. Under no flags this
    // is the identity on an already-open set, which is what keeps
    // `--as-of <now>` byte-identical to no flag at all.
    let mut current: Vec<Fact> = Vec::new();
    let mut closed: Vec<Fact> = Vec::new();
    for f in facts {
        let iv = Interval::of(&f);
        if args.as_of.is_some() {
            if contains(&iv, as_of) {
                current.push(f);
            } else {
                closed.push(f);
            }
        } else if iv.is_open() {
            current.push(f);
        } else {
            closed.push(f);
        }
    }

    // Deterministic order: safety relations first (a caller scanning the top
    // of the output must see the slots that can hurt someone), then
    // alphabetical; inside a group by member, then newest interval, then id.
    // Two runs on one store must byte-match.
    current.sort_by(|a, b| {
        let (ca, cb) = (
            canonical_relation(&a.predicate),
            canonical_relation(&b.predicate),
        );
        let (sa, sb) = (is_safety_slot(&ca), is_safety_slot(&cb));
        sb.cmp(&sa)
            .then_with(|| ca.cmp(&cb))
            .then_with(|| {
                member_identity(&ca, &a.object_norm).cmp(&member_identity(&cb, &b.object_norm))
            })
            .then_with(|| b.valid_from.cmp(&a.valid_from))
            .then_with(|| a.id.cmp(&b.id))
    });

    let total = current.len() as i64;
    let key = CursorKey::new(
        "state",
        &[
            ("kb", kb_filter),
            ("diary", diary_filter),
            ("subject", Some(subject_norm.as_str())),
            ("relation", relation.as_deref()),
            ("as_of", args.as_of.as_deref()),
            ("closed", Some(if args.include_closed { "1" } else { "0" })),
        ],
    );
    let offset = match &args.cursor {
        Some(raw) => read_surface::decode_cursor(raw, &key)?,
        None => 0,
    };

    // GROUPS COME FROM THE WHOLE CURRENT SET; only VALUES are paged. Paging
    // runs over facts, so a group skeleton built from the page would silently
    // drop every relation whose facts fell past the limit — `state ada
    // --limit 1` answering "penicillin allergy" with no medication group at
    // all is an agent reading "no medications known". Every relation open at
    // `as_of` therefore always appears, carrying its true `open_count`; a
    // relation whose values all fell on a later page shows `values: []`
    // beside a non-zero `open_count`, which is a page marker, not an absence.
    let mut groups: Vec<RelationGroup> = Vec::new();
    let mut index: BTreeMap<String, usize> = BTreeMap::new();
    for f in &current {
        let canonical = canonical_relation(&f.predicate);
        match index.get(&canonical) {
            Some(i) => groups[*i].open_count += 1,
            None => {
                index.insert(canonical.clone(), groups.len());
                let group_questions: Vec<Value> = questions
                    .iter()
                    .filter(|q| q["relation"] == json!(canonical))
                    .cloned()
                    .collect();
                groups.push(RelationGroup {
                    spec: relation_spec(&canonical),
                    relation: canonical,
                    open_count: 1,
                    values: Vec::new(),
                    open_questions: group_questions,
                    history: Vec::new(),
                });
            }
        }
    }

    let page: Vec<Fact> = current
        .into_iter()
        .skip(offset)
        .take(args.limit.max(1))
        .collect();
    let returned = page.len();
    let cursor = read_surface::next_cursor(&key, offset, returned, total);

    // History is keyed by the SLOT a closed interval belonged to, not by its
    // object: on a single-valued relation the slot is the relation itself, so
    // "lived in Lisbon" attaches to "lives in Porto" — the value that replaced
    // it. Keying by member there would file every superseded value under a
    // member no open value has, and `--include-closed` would silently return
    // no history at all.
    let mut history: HashMap<String, Vec<Fact>> = HashMap::new();
    if args.include_closed {
        for f in closed {
            history.entry(history_key(&f)).or_default().push(f);
        }
        for rows in history.values_mut() {
            rows.sort_by(|a, b| {
                b.valid_until
                    .cmp(&a.valid_until)
                    .then_with(|| a.id.cmp(&b.id))
            });
        }
    }

    // Fill the paged values into their skeletons. Every paged fact's relation
    // is already in `index` because the page is a subset of `current`.
    let mut attached: HashSet<Uuid> = HashSet::new();
    for f in page {
        let canonical = canonical_relation(&f.predicate);
        let spec = relation_spec(&canonical);
        let member = member_identity(&canonical, &f.object_norm);
        let contested = if contested_capped {
            None
        } else {
            Some(contested_ids.contains(&f.id))
        };
        let basis = confidence_for(&f, &spec, contested, as_of);
        let confidence = state_confidence(&basis);
        let provenance = provenance_for(ctx, &f)?;
        let own_history: Vec<Fact> = history.get(&history_key(&f)).cloned().unwrap_or_default();
        attached.extend(own_history.iter().map(|h| h.id));
        let value = StateValue {
            member,
            confidence,
            basis,
            provenance,
            history: own_history,
            fact: f,
        };
        let i = index[&canonical];
        groups[i].values.push(value);
    }

    // Closed intervals that no open value inherited — a medication with no
    // successor, a relation whose every value is now closed — are attached to
    // their relation instead of being dropped. `--include-closed` that
    // silently loses the ended intervals is the flag lying about its name.
    if args.include_closed {
        for rows in history.values() {
            for f in rows {
                if attached.contains(&f.id) {
                    continue;
                }
                let canonical = canonical_relation(&f.predicate);
                let spec = relation_spec(&canonical);
                match index.get(&canonical) {
                    Some(i) => groups[*i].history.push(f.clone()),
                    None => {
                        index.insert(canonical.clone(), groups.len());
                        groups.push(RelationGroup {
                            relation: canonical,
                            spec,
                            open_count: 0,
                            values: Vec::new(),
                            open_questions: Vec::new(),
                            history: vec![f.clone()],
                        });
                    }
                }
            }
        }
        for g in &mut groups {
            g.history.sort_by(|a, b| {
                b.valid_until
                    .cmp(&a.valid_until)
                    .then_with(|| a.id.cmp(&b.id))
            });
        }
    }

    // A fuzzy answer is about a DIFFERENT NAME than the caller typed, so the
    // near-miss list ships with results too, not only with an absence. A
    // RESOLVED subject with no open values gets no near-miss list at all:
    // "did you mean ada (1.00)" under an answer about ada is the certificate
    // arguing with itself.
    let subject_known = !matches!(resolved_from, SubjectResolution::Raw);
    let want_nearest = matches!(resolved_from, SubjectResolution::Fuzzy { .. })
        || (groups.is_empty() && !subject_known);
    let nearest = if want_nearest {
        nearest_subjects(ctx, diary_filter, kb_filter, &normalize(&args.subject), 5)?
    } else {
        Vec::new()
    };
    // "No allergy data for ada" and "no such person as ada" must never be the
    // same answer: the first is a fact about the store, the second about the
    // question. Resolution already told us which one this is — `Raw` means no
    // exact, alias or fuzzy branch matched any fact-bearing subject.
    let absence = if groups.is_empty() {
        Some(if subject_known {
            // No fabricated scores: nothing was rejected by a cutoff here —
            // the subject MATCHED and simply holds nothing open in this
            // scope, for this relation, at this as-of.
            Absence::new("no_open_values", scanned_kbs.clone(), 0)
        } else {
            Absence::new("subject_not_found", scanned_kbs.clone(), 0)
                .with_scores(
                    nearest.first().map(|n| n.score as f32),
                    Some(FUZZY_THRESHOLD as f32),
                )
                .with_nearest(nearest.clone())
        })
    } else {
        None
    };

    Ok(StateView {
        subject: display,
        subject_norm,
        resolved_from,
        as_of,
        groups,
        open_questions: questions,
        returned,
        total,
        cursor,
        scanned_kbs,
        diary_label: args.diary.clone(),
        nearest_subjects: nearest,
        absence,
    })
}

impl StateView {
    pub fn to_json(&self) -> Value {
        let state: Vec<Value> = self
            .groups
            .iter()
            .map(|g| {
                json!({
                    "relation": g.relation,
                    "arity_class": arity_class(&g.spec),
                    "safety": g.spec.is_safety(),
                    "mutability": mutability_str(&g.spec),
                    "temporality": temporality_str(&g.spec),
                    "value_domain": value_domain_str(&g.spec),
                    "open_count": g.open_count,
                    "values": g.values.iter().map(|v| json!({
                        "handle": read_surface::handle(HandleKind::Fact, v.fact.id),
                        "id": v.fact.id.to_string(),
                        "object": v.fact.object,
                        "object_norm": v.fact.object_norm,
                        "member": v.member,
                        "kb": v.fact.kb,
                        "diary": v.fact.diary,
                        "valid_from": v.fact.valid_from.map(|t| t.to_rfc3339()),
                        "valid_until": v.fact.valid_until.map(|t| t.to_rfc3339()),
                        "open": engram_core::interval::is_open_fact(&v.fact),
                        "confidence": v.confidence,
                        "confidence_basis": v.basis.to_json(),
                        "source_kind": v.fact.source_kind.as_str(),
                        "confirmations": v.fact.confirmations,
                        "provenance": provenance_json(&v.provenance),
                        "history": v.history.iter().map(history_json).collect::<Vec<_>>(),
                    })).collect::<Vec<_>>(),
                    "history": g.history.iter().map(history_json).collect::<Vec<_>>(),
                    "open_questions": g.open_questions,
                })
            })
            .collect();

        let mut payload = Map::new();
        payload.insert("subject".to_string(), json!(self.subject));
        payload.insert("subject_norm".to_string(), json!(self.subject_norm));
        payload.insert(
            "resolved_from".to_string(),
            json!(self.resolved_from.as_str()),
        );
        if let SubjectResolution::Fuzzy { score } = self.resolved_from {
            payload.insert("resolution_score".to_string(), json!(score));
        }
        payload.insert("as_of".to_string(), json!(self.as_of.to_rfc3339()));
        payload.insert("state".to_string(), Value::Array(state));
        payload.insert(
            "open_questions".to_string(),
            Value::Array(self.open_questions.clone()),
        );
        // Present on a fuzzy MATCH as well as on a miss: an agent that only
        // reads `state[]` must still be able to see, in a field it does read,
        // that it was answered about Adam when it asked about Ada.
        if !self.nearest_subjects.is_empty() {
            payload.insert("nearest_subjects".to_string(), json!(self.nearest_subjects));
        }
        Listing::page(self.returned, self.total, self.cursor.clone()).attach(&mut payload);
        match &self.absence {
            Some(a) => Verdict::NoMatch(a.clone()).attach(&mut payload),
            None => Verdict::Results.attach(&mut payload),
        }
        Value::Object(payload)
    }

    fn metadata(&self) -> Metadata {
        let mut meta = Metadata::default();
        let origin = if self.scanned_kbs.len() == 1 {
            "flag"
        } else {
            "all_kbs"
        };
        // `Scope::listing`, not `retrieval`: `state` scores no chunks, so
        // `chunks_considered` is omitted rather than reported as 0.
        Scope::listing(self.scanned_kbs.clone(), self.diary_label.clone(), origin)
            .attach(&mut meta);
        meta.add("subject_resolution", self.resolved_from.as_str());
        meta
    }
}

/// `engram state <subject>`.
pub fn run(ctx: &AppContext, args: StateArgs) -> Result<(), CliError> {
    let view = compile_state(ctx, &args)?;
    let meta = view.metadata();
    print_success(ctx.format, view.to_json(), meta, |data| {
        let subject = data["subject"].as_str().unwrap_or("");
        if data["verdict"] == json!("no_match") {
            println!("(nothing known about {subject} in this scope)");
            if let Some(near) = data["absence"]["nearest_subjects"].as_array() {
                if !near.is_empty() {
                    println!("Did you mean:");
                    for n in near {
                        println!(
                            "  {} ({:.2})",
                            n["subject"].as_str().unwrap_or(""),
                            n["score"].as_f64().unwrap_or(0.0)
                        );
                    }
                }
            }
            return;
        }
        if data["resolved_from"] == json!("fuzzy") {
            println!("(answering about `{subject}`, the nearest known subject to what you typed)");
        }
        println!(
            "State of {subject} as of {}:",
            data["as_of"].as_str().unwrap_or("")
        );
        for group in data["state"].as_array().cloned().unwrap_or_default() {
            let safety = if group["safety"] == json!(true) {
                " [safety]"
            } else {
                ""
            };
            println!("  {}{}", group["relation"].as_str().unwrap_or(""), safety);
            let shown = group["values"].as_array().map(|v| v.len()).unwrap_or(0);
            let open = group["open_count"].as_u64().unwrap_or(0) as usize;
            if shown < open {
                println!("    ({shown} of {open} open values on this page — follow the cursor)");
            }
            for v in group["values"].as_array().cloned().unwrap_or_default() {
                println!(
                    "    {}  (conf={:.2}, since {})",
                    v["object"].as_str().unwrap_or(""),
                    v["confidence"].as_f64().unwrap_or(0.0),
                    v["valid_from"].as_str().unwrap_or("unknown")
                );
                for h in v["history"].as_array().cloned().unwrap_or_default() {
                    println!(
                        "      was: {}  (until {}; {})",
                        h["object"].as_str().unwrap_or(""),
                        h["valid_until"].as_str().unwrap_or("?"),
                        h["close_reason"].as_str().unwrap_or("closed")
                    );
                }
            }
            for h in group["history"].as_array().cloned().unwrap_or_default() {
                println!(
                    "    (ended) {}  (until {}; {})",
                    h["object"].as_str().unwrap_or(""),
                    h["valid_until"].as_str().unwrap_or("?"),
                    h["close_reason"].as_str().unwrap_or("closed")
                );
            }
        }
        let questions = data["open_questions"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        if !questions.is_empty() {
            println!();
            println!("Open questions ({}):", questions.len());
            for q in &questions {
                println!(
                    "  {} -- {} vs {}  -> {}",
                    q["relation"].as_str().unwrap_or(""),
                    q["old"]["object"].as_str().unwrap_or(""),
                    q["new"]["object"].as_str().unwrap_or(""),
                    q["resolve_command"].as_str().unwrap_or("")
                );
            }
        }
        println!();
        println!(
            "{} of {} open values",
            data["returned"].as_i64().unwrap_or(0),
            data["total"].as_i64().unwrap_or(0)
        );
        if let Some(c) = data["cursor"].as_str() {
            println!("more: engram state {subject} --cursor {c}");
        }
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn iv(from: &str, until: Option<&str>) -> Interval {
        let p = |s: &str| {
            DateTime::parse_from_rfc3339(s)
                .expect("rfc3339")
                .with_timezone(&Utc)
        };
        Interval {
            from: p(from),
            until: until.map(p),
        }
    }

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s)
            .expect("rfc3339")
            .with_timezone(&Utc)
    }

    #[test]
    fn a_point_interval_holds_from_its_instant_onward() {
        // M14: the reviewed draft returned `from == t`, which deletes every
        // Instant relation from every --as-of query. A birthday is true from
        // the day it happened, forever.
        let birthday = iv("1988-04-02T00:00:00Z", Some("1988-04-02T00:00:00Z"));
        assert!(birthday.is_point());
        assert!(contains(&birthday, at("1988-04-02T00:00:00Z")));
        assert!(contains(&birthday, at("2026-01-01T00:00:00Z")));
        assert!(!contains(&birthday, at("1987-01-01T00:00:00Z")));
    }

    #[test]
    fn a_state_interval_is_half_open() {
        let taking = iv("2024-01-01T00:00:00Z", Some("2025-01-01T00:00:00Z"));
        assert!(contains(&taking, at("2024-06-01T00:00:00Z")));
        assert!(
            !contains(&taking, at("2025-01-01T00:00:00Z")),
            "until is exclusive"
        );
        assert!(!contains(&taking, at("2026-01-01T00:00:00Z")));
        let open = iv("2024-01-01T00:00:00Z", None);
        assert!(contains(&open, at("2099-01-01T00:00:00Z")));
    }

    #[test]
    fn confidence_is_the_sum_of_its_published_terms() {
        let basis = ConfidenceBasis {
            extraction: 0.85,
            source_kind: CONF_USER_STATED,
            confirmations: 0.04,
            staleness: -0.02,
            contested: Some(0.0),
        };
        assert!((state_confidence(&basis) - 0.92).abs() < 1e-5);
    }

    #[test]
    fn confidence_never_reaches_certainty_or_zero() {
        let certain = ConfidenceBasis {
            extraction: 1.0,
            source_kind: CONF_USER_STATED,
            confirmations: CONF_CONFIRMATION_CAP,
            staleness: 0.0,
            contested: Some(0.0),
        };
        assert_eq!(state_confidence(&certain), CONF_MAX);
        let hopeless = ConfidenceBasis {
            extraction: 0.0,
            source_kind: CONF_COMPILE_LLM,
            confirmations: 0.0,
            staleness: CONF_STALENESS_CAP,
            contested: Some(CONF_CONTESTED),
        };
        assert_eq!(state_confidence(&hopeless), CONF_MIN);
    }

    #[test]
    fn an_unlooked_contested_term_contributes_nothing_and_stays_null() {
        let basis = ConfidenceBasis {
            extraction: 0.8,
            source_kind: 0.0,
            confirmations: 0.0,
            staleness: 0.0,
            contested: None,
        };
        assert!((state_confidence(&basis) - 0.8).abs() < 1e-5);
        assert!(basis.to_json()["contested"].is_null());
    }

    #[test]
    fn a_wildcard_scope_is_never_a_name() {
        assert_eq!(scope_filter("*"), None);
        assert_eq!(scope_filter(" * "), None);
        assert_eq!(scope_filter(""), None);
        assert_eq!(scope_filter("work"), Some("work"));
    }

    #[test]
    fn as_of_accepts_a_bare_date_and_refuses_prose() {
        assert!(parse_as_of(&Some("2026-03-01".into())).unwrap().is_some());
        assert!(parse_as_of(&Some("2026-03-01T12:00:00Z".into()))
            .unwrap()
            .is_some());
        let err = parse_as_of(&Some("last tuesday".into())).unwrap_err();
        assert_eq!(err.exit_code(), 3);
    }
}

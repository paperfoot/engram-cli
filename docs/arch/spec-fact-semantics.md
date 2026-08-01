# spec — fact-semantics lane (engram v0.4)

Implements **move 2** (bi-temporal interval facts) and the **replay half of move 3** (decision
replay on re-derivation) from `docs/arch/redesign-brief.md`. Reads `docs/arch/interfaces.md` as
the cross-lane contract; every divergence request is listed in §6 rather than invented here.

Owned files (exclusive): `crates/engram-graph/src/**`, `crates/engram-cli/src/fact_write.rs`,
`crates/engram-cli/src/fact_llm.rs`, and new tests matching `crates/engram-cli/tests/wave3_fact_*.rs`.

---

## 1. Scope and non-goals

### 1.1 What this lane delivers

1. **A relation registry** — one versioned table that answers, for a canonical relation:
   arity (set-valued vs single-valued), safety class, mutability, temporality, value domain.
   `takes_medication`, `allergic_to`, `diagnosed_with` become **safety-critical SETS**;
   `blood_type` stays a **safety-critical single value**; `birthday`, `died_on`, `nationality`
   stay single-valued and **immutable**. Replaces the accidental arity derivation at
   `fact_write.rs:136-142` (`slot_policy(...).is_some() → Single`), which is the root cause of
   the polypharmacy storm.
2. **Interval algebra** over `(valid_from, valid_until)` — overlap, disjointness, point
   intervals, open ends, reinstatement, and an **interval-closing safety gate**: closing an
   open interval on a safety relation requires authorizing evidence, and heuristics are never
   authorizing.
3. **A rewritten `classify_pair`** whose default answer is coexistence. Contradiction is
   reached only by (a) temporal incompatibility of a single-valued relation, (b) same-member
   disagreement on a set-valued relation, (c) disagreement on an immutable attribute.
   Different medications never form a pair at all — the storm dies at pair construction, not at
   classification.
4. **`suggested_action` replacement** — the suggestion vocabulary shrinks to
   `keep-both` | `requires_context`. `supersede-old` is never *suggested* again (it remains an
   accepted `facts resolve --action` value, forever, per constraint 3). Kills
   `fact_write.rs:1209-1213`.
5. **Decision replay** — decisions are keyed by SEMANTIC identity
   (`subject_norm`, `relation`, `object_pair_hash`), recorded as journal events, and replayed
   before any conflict is opened. A re-derivation after `edit`, `facts backfill` or `compile`
   never re-asks a question the caller already answered.
6. **A conversion plan** for the 951 open and ~14.5k resolved `fact_conflict_queue` rows into
   intervals plus decision events, with a measurement procedure that never touches the live store.

### 1.2 Non-goals (explicitly other lanes' or later waves')

- The `events` table, `append_event`, the v9→v10 migration body, `Handle`, and the `Arity` enum's
  home in `engram-core` — **storage-journal**. This spec states exactly what it needs from them
  (§6) and supplies the pure functions the migration calls.
- `facts.rs` command surface (`list`, `conflicts`, `triage`, `resolve`), `recall`'s conflicts
  array, envelope totals/cursors — **read-surfaces**. This lane keeps their imports compiling
  and hands them shared helpers instead of duplicated rules.
- `remember`, `edit`, `forget`, `import`, `export` call sites — **write-surfaces**.
- `compile.rs`'s single `ExtractedFact` struct literal — **daemon-jobs**, one coordinated line (§6.5).
- Async jobs, single-writer `Core`, HTTP parity — **daemon-jobs**.
- `engram state <subject>` — **reasoning-layer**; it consumes `open_intervals()` and
  `relation_spec()` from this lane and adds no semantics of its own.
- Entity resolution quality (`canonicalize_subject`, `hygiene.rs`, `people.rs`) is untouched.
  Subject identity stays exactly as good (or bad) as it is today; changing it in the same wave
  would make the interval results unattributable.
- No new LLM calls. Every rule added here is deterministic and offline; the extraction prompt
  gains fields but not a second round-trip.

---

## 2. Data model and API changes

### 2.1 New: `crates/engram-graph/src/relations.rs`

The registry. Pure, no I/O, versioned so a conflict row can record which registry classified it.

```rust
/// Bump when any classification below changes. Recorded on queue rows so a
/// backlog can be re-keyed after a registry change instead of re-litigated.
pub const REGISTRY_VERSION: &str = "relations@1";

/// How many values one (subject, relation) may hold at once, and whether a
/// wrong answer here is a medical incident.
///
/// Mirrors `engram_core::Arity` (interfaces.md §3) exactly; re-exported so
/// callers never see two spellings of the same idea.
pub use engram_core::types::Arity;   // { SetValued { safety: bool }, SingleValued { safety: bool } }

/// The OLD two-variant enum, which `fact_write.rs:78-85` calls `Arity` and
/// `commands/facts.rs:21` imports under that name. It cannot keep the name —
/// `Arity` now means the interfaces.md enum — so it is renamed here and
/// `fact_write.rs` re-exports it under BOTH spellings:
/// `pub use engram_graph::relations::LegacyArity as Arity;` plus
/// `pub use engram_graph::relations::LegacyArity;`. That one alias is what
/// keeps `commands/facts.rs:436-439`'s `Arity::Single | Arity::Multi` match
/// arms compiling untouched; the new enum is NOT re-exported from
/// `fact_write.rs`, so no read-surfaces file sees two meanings of one name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LegacyArity { Single, Multi }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mutability {
    /// The value can legitimately change over time (lives_in, works_at, takes_medication).
    Mutable,
    /// The value cannot change; two distinct values are a contradiction whatever
    /// their timestamps (birthday, blood_type, died_on).
    Immutable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Temporality {
    /// A state that persists over an interval: [valid_from, valid_until).
    State,
    /// A point in time; the interval is [t, t] and never overlaps another.
    Instant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueDomain { Date, Text }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RelationSpec {
    pub arity: Arity,
    pub mutability: Mutability,
    pub temporality: Temporality,
    pub value: ValueDomain,
}

impl RelationSpec {
    pub fn is_safety(&self) -> bool;          // matches!(arity, SetValued{safety:true} | SingleValued{safety:true})
    pub fn is_set_valued(&self) -> bool;
    pub fn is_single_valued(&self) -> bool;
}

/// The registry lookup. Input MUST be canonical (`canonical_relation`).
pub fn relation_spec(canonical: &str) -> RelationSpec;

/// interfaces.md §3 signature, implemented as `relation_spec(c).arity`.
pub fn relation_arity(canonical: &str) -> Arity;

/// Predicate synonyms + cessation forms folded onto one canonical relation.
/// Delegates to `facts::canonical_predicate`, then applies `cessation_of`.
pub fn canonical_relation(raw: &str) -> String;

/// `stopped_taking` / `no_longer_takes` / `discontinued` name the END of a
/// `takes_medication` interval, not a relation of their own. Returns the
/// relation whose interval such a predicate closes.
pub fn cessation_of(raw_or_canonical: &str) -> Option<&'static str>;

/// Legacy projection for surfaces that already publish `arity: "single"|"multi"`
/// (commands/facts.rs:436-439). Kept so read-surfaces keeps compiling and the
/// existing JSON value keeps its meaning.
pub fn predicate_arity(canonical: &str) -> LegacyArity;  // Single | Multi

/// The safety predicate `commands/facts.rs:440` and `:586-587` import from
/// `fact_write`. It MUST be re-expressed against the registry, not against the
/// deprecated `slot_policy` shim: today it is
/// `matches!(slot_policy(c), Some(AwaitConfirmation))` (fact_write.rs:145-147),
/// and under §2.5's shim the safety SETS map to `None`, so a shim-based
/// `is_safety_slot("allergic_to")` would return FALSE — flipping the
/// `"safety_slot"` JSON field to false for exactly the relations it exists to
/// flag, and silently disarming the triage defence at facts.rs:581-593.
/// `fact_write.rs` re-exports this name unchanged.
pub fn is_safety_slot(canonical: &str) -> bool;   // relation_spec(canonical).is_safety()
```

Registry contents (v1). Anything not listed is `SetValued { safety: false }`, `Mutable`,
`State`, `Text` — the unchanged default from `fact_write.rs:133-142`, and the safe one: a wrong
`Set` costs two coexisting facts, a wrong `Single` silently retires a true one.

| relation | arity | mutability | temporality | notes |
|---|---|---|---|---|
| `takes_medication` | SetValued{safety:true} | Mutable | State | **changed**: was single-valued |
| `allergic_to` | SetValued{safety:true} | Mutable | State | **changed** |
| `diagnosed_with` | SetValued{safety:true} | Mutable | State | **changed** |
| `blood_type` | SingleValued{safety:true} | Immutable | State | pinned by interfaces.md §3 |
| `birthday`, `has_birthday`, `date_of_birth`, `born_on` | SingleValued{false} | Immutable | Instant | Date domain |
| `died_on` | SingleValued{false} | Immutable | Instant | Date |
| `nationality`, `has_nationality` | SingleValued{false} | Immutable | State | |
| `lives_in`, `works_at`, `has_role`, `has_age`, `married_to`, `partner_of`, `prefers_language` | SingleValued{false} | Mutable | State | today's `LastWriterWins` set |
| `married_on`, `founded_on`, `graduated_on`, `joined_on`, `started_on`, `ended_on`, `hired_on` | SetValued{false} | Immutable | Instant | Date; events, not slots |
| every entry of today's `MULTI_VALUED_PREDICATES` (`fact_write.rs:103-131`) | SetValued{false} | Mutable | State | unchanged behaviour, now stated once |

`has_age` deserves a note in the code: it is single-valued and mutable, and its value is a
function of `birthday` plus today. It stays a slot; deriving it is reasoning-layer work.

**Member identity** (interfaces.md §3, `member_identity`) also lives here:

```rust
/// Bump when the heuristic changes; stored on queue rows so a re-key is possible.
pub const MEMBER_IDENTITY_VERSION: &str = "member_identity@1";

/// Which SET MEMBER an object names. "empagliflozin 12.5 mg daily" and
/// "empagliflozin 25mg" are the same member at different doses; "metformin" is
/// a different member entirely.
///
/// For single-valued relations the member IS the whole object — there is only
/// one slot to be in.
///
/// Errs toward MORE distinct members. Under-merging costs a second coexisting
/// interval; over-merging closes an interval that was true, or manufactures a
/// dose-change conflict out of two different drugs.
pub fn member_identity(canonical_relation: &str, object_norm: &str) -> String;
```

v1 algorithm, in order, on the already-normalized (lowercased, trimmed) object:

1. drop any trailing parenthetical — `penicillin (severe)` → `penicillin`;
2. cut at the first `,`, `;`, ` - `, `—`, or `:` and keep the head;
3. pop trailing **noise tokens** while the last token is one of:
   - a bare number or number+unit (`12.5`, `500`, `12.5mg`, `2x`, `1000iu`),
   - a unit (`mg mcg µg g ml l iu unit units tablet tablets tab caps capsule capsules puff puffs drop drops %`),
   - a frequency (`daily nightly weekly monthly bid tid qid qd hs prn once twice three four times per day week night morning evening every other`),
   - a route/form (`oral orally po iv im sc subq subcutaneous topical sublingual inhaled inhaler injection injected patch cream gel spray tablet`),
   - a qualifier (`stage grade type` only when the token popped before it was numeric; `allergy allergies` for `allergic_to`);
4. drop a leading `the`/`a`;
5. collapse internal whitespace; if the result is empty, return the full `object_norm`.

`type 2 diabetes` is safe: nothing at the END is noise. `hypertension stage 2` → `hypertension`.
`metformin xr` stays distinct from `metformin` — deliberately (rule: err toward distinctness).

### 2.2 New: `crates/engram-graph/src/interval.rs`

```rust
/// A fact's validity window. `until: None` means the interval is still open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Interval { pub from: DateTime<Utc>, pub until: Option<DateTime<Utc>> }

impl Interval {
    /// The window a stored fact asserts. `valid_from` falls back to
    /// `created_at` (post-v10 it is never NULL, but a v9 store read by a v10
    /// binary must not panic); `superseded_at` closes an interval that has
    /// `superseded_by` set but no `valid_until` (pre-v6 rows).
    pub fn of(f: &Fact) -> Interval;
    pub fn is_open(&self) -> bool;
    pub fn overlaps(&self, other: &Interval) -> bool;   // half-open [from, until)
    pub fn precedes(&self, other: &Interval) -> bool;   // self.until <= other.from
    pub fn is_point(&self) -> bool;                     // until == Some(from)
}

/// True when the stored row still asserts something: not retracted, not closed.
pub fn is_open_fact(f: &Fact) -> bool;   // valid_until.is_none() && superseded_by.is_none()

/// Why an interval is being closed. Persisted in `facts.close_reason` and in
/// the `fact.interval_closed` payload (interfaces.md §2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseReason { Superseded, Stopped, Corrected, RetractedSource }

/// WHO is entitled to close it. This is the safety gate's only input besides
/// the relation spec — the gate does not care how confident a heuristic was.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CloseAuthority {
    /// The source text says the state ended ("stopped taking X in March").
    ExplicitEnd,
    /// A human or the calling agent adjudicated this exact pair.
    Caller { actor: String },
    /// A prior CALLER decision on the same semantic key, replayed.
    ReplayedCaller { decided_at: DateTime<Utc> },
    /// A later interval on a single-valued relation implies the earlier one ended.
    SuccessorInterval,
    /// Recency, confidence gap, ingestion order, LLM adjudication.
    Heuristic { rule: &'static str },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CloseVerdict { Allow, Refuse { reason: &'static str } }

/// THE interval-closing safety gate. One function, called by every path that
/// could close an interval: the write path, `facts triage`, `facts resolve`,
/// and the compiler.
///
/// A safety relation may be closed only by evidence that a person supplied:
/// an explicit end in the text, a caller decision, or a replayed caller
/// decision. `SuccessorInterval` and `Heuristic` are refused — a second
/// medication is not evidence that the first one stopped, and neither is a
/// newer timestamp.
///
/// An immutable relation additionally refuses `SuccessorInterval`: nobody
/// acquires a second birthday.
pub fn may_close(spec: &RelationSpec, authority: &CloseAuthority) -> CloseVerdict;

/// The instant an interval closes when it is superseded by a successor: the
/// successor's `valid_from`, clamped so a close can never precede its own
/// `valid_from` (a negative-length interval is unreadable by every consumer).
/// `close_at(old, s) = max(old.from, s)`. The v9→v10 migration (§4.1) applies
/// the SAME clamp — see the note there; the clamp is not optional in one place
/// and advisory in the other.
pub fn close_at(old: &Interval, successor_from: DateTime<Utc>) -> DateTime<Utc>;
```

**Point-interval algebra, stated so nobody re-derives it.** `overlaps` is half-open
`[from, until)`, so a strict reading makes `[t, t)` empty and a `Temporality::Instant` interval
overlaps nothing — including itself. That is the wanted answer for "does this instant collide
with a state interval", and the wrong answer for "are these two facts about the same instant".
So:

- `Interval::is_point()` is `until == Some(from)`.
- `overlaps` treats a point as empty: a point interval never overlaps anything, per test 10.
- `Instant` relations therefore never reach the interval branch (§2.3.1 step 8) at all —
  every `Instant` relation in the registry is also `Immutable`, so two different values are
  settled by step 7 (`immutable_attribute_disagreement`) and two identical values by step 4
  (`duplicate_object`). If a future registry entry is `Instant` + `Mutable`, step 8 must be
  extended before it lands; a debug assertion in `relation_spec`'s tests pins that today no such
  entry exists.

### 2.3 New: `crates/engram-graph/src/semantics.rs`

`classify_pair` and everything around it move here from `fact_write.rs:73-354` so read-surfaces,
daemon-jobs and reasoning-layer can use them without depending on `engram-cli`. `fact_write.rs`
re-exports every moved name, so `commands/facts.rs:20-24` keeps compiling untouched.

```rust
// ---- unchanged public shapes (re-exported from fact_write.rs) ----
pub enum ConflictClass { Coexist, Duplicate, SupersedeOld, SupersedeNew, Settled, Queue }
pub struct Verdict { pub class: ConflictClass, pub rule: &'static str }
pub enum ClassifyMode { Write, Triage }
impl Verdict { pub fn is_auto_resolvable(&self) -> bool; }
```

`SupersedeOld` / `SupersedeNew` keep their names and their JSON spellings
(`commands/facts.rs:395-404`) but are **redefined as interval closes**: the loser keeps its row,
gains `valid_until`, `close_reason` and `closed_by`, and stays queryable. Nothing is retracted.
Keeping the variant names is deliberate: renaming them would break another lane's match arms and
change `resolved_by_rule` histogram keys for zero semantic gain.

```rust
/// Everything the classifier needs about ONE side of a pair, resolved once so
/// the registry and the member heuristic are not re-run per comparison.
pub struct FactView<'a> {
    pub fact: &'a Fact,
    pub relation: String,       // canonical
    pub spec: RelationSpec,
    pub member: String,         // member_identity(relation, object_norm)
    pub interval: Interval,
}
impl<'a> FactView<'a> { pub fn of(f: &'a Fact) -> FactView<'a>; }

/// Classify one candidate pair. Pure, side-effect free, and the ONLY place
/// contradiction is defined — the write path, `facts triage` and `engram state`
/// all read this one function.
pub fn classify_pair(new: &Fact, old: &Fact, score: Option<f32>, mode: ClassifyMode) -> Verdict;

/// Same, without re-deriving the views (hot path: one new fact against N siblings).
pub fn classify_views(new: &FactView<'_>, old: &FactView<'_>, score: Option<f32>, mode: ClassifyMode) -> Verdict;

/// Defence in depth, exported so no caller has to re-implement it: downgrade
/// any interval-closing verdict on a safety relation to Queue. Called at the
/// end of `classify_pair` AND available to `facts triage`
/// (commands/facts.rs:581-593 replaces its inline copy with this).
pub fn safety_gate(v: Verdict, new: &FactView<'_>, old: &FactView<'_>) -> Verdict;

/// What the CALLER should be told to do. Never `supersede-old`.
pub fn suggested_action(v: &Verdict, new: &FactView<'_>, old: &FactView<'_>) -> &'static str;

/// The actions `facts resolve` will accept for this pair, for the envelope's
/// `resolve.allowed_actions` (interfaces.md §4).
pub fn allowed_actions(v: &Verdict, new: &FactView<'_>, old: &FactView<'_>) -> &'static [&'static str];
```

#### 2.3.1 The decision procedure

Evaluated in this order. Rule names in `code` are the values of `Verdict.rule`; names marked
NEW did not exist in v0.3.1.

1. `new.subject_norm != old.subject_norm` → `Coexist("different_subject")`.
   One person's allergy is not evidence about another's; this verdict touches no row, so it is
   safe even for safety relations. (Unchanged; still first, still the cheapest.)
2. `old.superseded_by.is_some() || new.superseded_by.is_some()` → `Settled("already_settled")`.
   **Moved ahead of the safety check.** In v0.3.1 the safety early-return at `fact_write.rs:245`
   fired first, so a safety pair whose loser had already been retired stayed in the queue
   forever with nothing left to decide. That class is a measurable slice of the 951.
3. Relations differ:
   - both single-valued and `score >= PARAPHRASE_SIMILARITY (0.88)` → `Queue("paraphrase_slot")`;
   - otherwise → `Coexist("different_predicate")`.
4. Same relation, `new.object_norm == old.object_norm`:
   - `old` interval open → `Duplicate("duplicate_object")` (confirmation; allowed on safety
     relations — counting a re-assertion closes nothing);
   - `old` interval closed and `new.interval.from >= old.interval.until` →
     `Coexist("reinstated_interval")` NEW. Restarting a medication after stopping it is a second
     interval, not a duplicate and not a contradiction.
   - `old` interval closed and `new.interval.from < old.interval.until` →
     `Settled("restates_closed_interval")` NEW. A backdated assertion that lands INSIDE an
     already-closed window is not news: the store already recorded that the state held then, and
     re-opening it would resurrect a value someone deliberately ended. Without this arm the pair
     falls through into steps 5–8 and can be closed or queued on the strength of a clock, which
     is the failure Grok's review flagged. `Settled` touches no row.
5. Same relation, `spec.is_set_valued()`, **different member identity** →
   `Coexist("multi_valued")`, or `Coexist("safety_set_coexist")` NEW when either side is safety.
   **This branch runs before any safety consideration** — the inversion of
   `fact_write.rs:245` vs `:267` that produced 10 queued pairs from 5 medications. In practice
   the write path never even builds these pairs (§2.4 step 6), so this rule mostly serves
   `facts triage` draining the existing backlog.
6. Same relation, `spec.is_set_valued()`, **same member, different object** (a dose change):
   - safety → `Queue("safety_member_change")` NEW;
   - non-safety, with an event-time ordering → `SupersedeOld("member_attribute_change")` NEW
     (interval close, reason `Superseded`);
   - non-safety, unordered → fall through to step 8's ordering rules.
7. Same relation, `spec.is_single_valued()` and `spec.mutability == Immutable`:
   - `mode == Write` and `new.source_kind ∈ {UserStated, UserConfirmed}` and
     `old.source_kind ∈ {AgentExtracted, CompileLlm, Import}` →
     `SupersedeOld("correction_by_user")` NEW. A person correcting their own birthday is the one
     ordering signal an immutable attribute has. **It does not survive step 9 on a safety
     relation**: `blood_type` is `SingleValued{safety:true}`, so a blood-type correction is
     downgraded to `Queue("safety_slot")` like every other safety supersession. That is
     deliberate — a corrected blood type is exactly the value worth one confirmation — but it
     means `correction_by_user` fires only on `birthday`/`died_on`/`nationality`.
     **It also does not fire today at all**: `remember.rs:632` stamps every written fact
     `SourceKind::AgentExtracted`, including `--facts-json`, so no production path produces
     `UserStated`/`UserConfirmed`. The rule is written now so the classifier is right when
     write-surfaces starts stamping source kinds (§6.7), and its unit test (test 21) constructs
     the source kinds directly rather than going through `remember`;
   - otherwise → `Queue("immutable_attribute_disagreement")` NEW. Two birthdays are a
     contradiction no timestamp can settle, so recency must never touch them.
8. Same relation, single-valued, mutable — the interval rules:
   - `!new.interval.overlaps(&old.interval)` → `Coexist("disjoint_intervals")` NEW.
     London 2015–2020 and Lisbon 2024–now are both true; superseding one loses history the
     interval model exists to keep. This is the largest single-valued reduction.
   - overlapping, different source memory, `new.from - old.from >= 1s` →
     `SupersedeOld(write_supersede_rule | "recency_supersede")` — close `old` at
     `close_at(old, new.from)`.
   - overlapping, different source, `new.from - old.from <= -1s` → `SupersedeNew("recency_stale")`.
   - same source memory → `Queue("same_source_contradiction")`.
   - `mode == Write` → `SupersedeOld("slot_lww" | "exact_match")` (ingestion order is evidence
     when an agent is asserting a value now; unchanged).
   - confidence gap ≥ 0.3 → `SupersedeOld("confidence_gap")` / ≤ −0.3 → `SupersedeNew("confidence_gap")`.
   - otherwise → `Queue("comparable_values")`.
9. **Safety gate** (`safety_gate`), applied to the result of 4–8: if either side is safety and
   the class is `SupersedeOld`, `SupersedeNew` **or `Duplicate`**, downgrade to
   `Queue("safety_slot")`.
   The rule name stays `"safety_slot"` on purpose — `commands/facts.rs:701` reads
   `kept_by_reason["safety_slot"]` to populate `safety_slots_kept`, and a rename would silently
   report zero. `Coexist` and `Settled` pass through untouched.

   `Duplicate` is in the gate because `Duplicate` is a WRITE today, not a no-op: both
   `apply_write_verdict` (`fact_write.rs:1137-1146`) and `facts triage`
   (`commands/facts.rs:635-643`) resolve it by `supersede_fact_with(new, old)` — retiring the
   incoming row so the referenced id stays stable. The inline defence at `facts.rs:581-593` that
   §6.4.1 replaces already downgrades `Duplicate` on safety pairs, so leaving it out of
   `safety_gate` would let this lane INTRODUCE a heuristic close on a safety relation while
   claiming to remove them. Two consequences, both required:
   - the gate covers `Duplicate`, and
   - `apply_plan` implements duplicates as `confirm` only (`increment_fact_confirmations`, no
     `supersede_fact_with`, no `valid_until`), so a confirmation on a NON-safety relation stops
     writing `superseded_by` too. `outcome.deduped` and `outcome.added` keep their existing
     arithmetic; the difference is that the absorbed observation is never inserted rather than
     inserted and immediately retired (`WritePlan.insert == None`, §2.3.3).

`suggested_action`:

```
either side safety                                  -> "requires_context"
rule in { same_source_contradiction, comparable_values,
          paraphrase_slot, immutable_attribute_disagreement }
                                                    -> "requires_context"
otherwise                                           -> "keep-both"
```

`allowed_actions` is `["keep-both", "supersede-old", "reject-new"]` for every pair — a caller
decision authorizes a safety close (that is what `CloseAuthority::Caller` means), so the
vocabulary is not narrowed; only the *suggestion* is. `close-old` is added to the list when
read-surfaces ships it (§6.4).

#### 2.3.2 Decision keys and replay

```rust
pub const SEMANTIC_KEY_VERSION: &str = "semantic_key@1";

/// interfaces.md §3, verbatim.
pub struct SemanticKey { pub subject_norm: String, pub relation: String, pub object_pair_hash: String }

impl SemanticKey {
    /// object_pair_hash = hex(blake3(min(a,b) || 0x1f || max(a,b))) over the
    /// two object_norms, so the key is the same whichever fact arrives first.
    pub fn for_pair(new: &FactView<'_>, old: &FactView<'_>) -> SemanticKey;
    /// Stable single-column encoding for the queue row and the event payload:
    /// "<subject_norm>\u{1f}<relation>\u{1f}<object_pair_hash>".
    pub fn encode(&self) -> String;
    pub fn decode(s: &str) -> Option<SemanticKey>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecisionVerdict {
    /// `facts resolve --action keep-both`, or a coexistence verdict from a sweep.
    KeepBoth,
    /// `--action supersede-old`: the older interval closes.
    CloseOld,
    /// `--action reject-new`: the incoming interval closes.
    RejectNew,
    /// The question stopped existing without being answered: its facts were
    /// cascaded away (`delete_facts_for_memory`), or a migration could not
    /// reconstruct a verdict. `Void` is RECORDED so the row's history survives
    /// and NEVER replays — `replay` returns `None` for it. It exists so the
    /// migration (§4.1 step 4) and the open-key collapse (§4.1 step 5) have an
    /// honest terminal state that is not a silent `keep_both`.
    Void,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecisionAuthority {
    /// A human or the calling agent, via `facts resolve`.
    Caller,
    /// A classifier sweep (`facts triage`, the write path, the compiler).
    Machine,
}

pub struct DecisionRecord {
    pub key: SemanticKey,
    pub verdict: DecisionVerdict,
    pub authority: DecisionAuthority,
    pub actor: String,
    pub decided_at: DateTime<Utc>,
    pub note: Option<String>,
    pub key_version: String,
}

/// The store, behind a trait so replay is unit-testable without SQLite.
/// Implemented for `SqliteStore` in this crate (local trait, foreign type —
/// legal, and it keeps the dependency direction graph -> storage).
///
/// The method is `binding_decision`, NOT `lookup_decision`: `SqliteStore` gains
/// an inherent `lookup_decision(kb, key_encoded) -> Option<DecisionRow>` (§6.2)
/// and an inherent method shadows a trait method of the same name at every call
/// site. One name, one meaning. The trait impl encodes the key, calls the
/// inherent method, and maps the row.
pub trait DecisionLedger {
    fn binding_decision(&self, kb: &str, key: &SemanticKey) -> Result<Option<DecisionRecord>, StorageError>;
}

/// What a prior decision does to a freshly classified pair.
///
/// A CALLER decision is binding in every direction: keep-both suppresses the
/// question, close-old and reject-new re-apply it silently.
///
/// A MACHINE decision is binding ONLY when it is `KeepBoth`, because that
/// outcome closes nothing. Replaying a machine `CloseOld` would re-freeze the
/// exact classifier bug this redesign exists to remove: 14.5k of the resolved
/// rows were settled by `facts triage` under the old rules, and those rows must
/// never be able to retire a fact a second time.
pub fn replay(prior: &DecisionRecord, v: Verdict) -> Option<Verdict>;
// Some(Coexist("replayed_keep_both")) | Some(SupersedeOld("replayed_close_old"))
// | Some(SupersedeNew("replayed_reject_new")) | None  (no binding decision; classify normally)
```

**Which decision, when there are several.** `fact_decisions` accumulates rows per key: a triage
sweep records `machine` verdicts, a caller may answer the same question later, a later sweep
records another `machine` verdict. "Most recent wins" is wrong here — the newest row is usually a
machine one, and per the rule above a machine `close_old` is not binding, so `replay` would return
`None` and silently discard a binding caller decision underneath it. The precedence is therefore:

1. the most recent `authority = caller` row for `(kb, decision_key)`, if any;
2. otherwise the most recent `authority = machine` row;
3. `Void` rows are never selected — they are history, not a verdict.

`binding_decision` implements exactly that; `idx_fact_decisions_key` (§2.6) gains `authority` so
the lookup is one index scan. Test 28 asserts the caller row wins over a newer machine row.

**When the lookup fails.** `binding_decision` returns `Result`, and a `SqliteBusy` must abort the
write, not degrade to "no prior decision". Treating a failed lookup as absence re-asks a settled
safety question at best and re-closes an interval the caller reinstated at worst. `plan_write`'s
`prior` closure therefore propagates the error (§2.3.3).

Replay runs **before a conflict row is opened**, in `plan_write` (§2.4) and in
`facts triage`. Because the key is semantic, a re-derived fact with a fresh UUID after
`edit --content` hits the same key as the fact it replaced — which is the whole point of move 3.

#### 2.3.3 The pure write planner

The write path splits into a pure planner (here) and an applier (`fact_write.rs`). This is what
makes the polypharmacy guarantee testable without a store: assert on a `WritePlan` built from
fixtures.

```rust
/// ONE incoming assertion, already canonicalized by the applier (§2.4 steps
/// 2-5) so the planner does no I/O and no entity resolution.
pub struct Observation {
    pub fact_id: Uuid,              // pre-minted so PlannedConflict can name it
    pub subject: String,
    pub subject_norm: String,
    pub relation: String,           // canonical
    pub spec: RelationSpec,
    pub object: String,
    pub object_norm: String,
    pub member: String,             // member_identity(relation, object_norm)
    pub confidence: f32,
    pub source_memory_id: Uuid,
    pub source_kind: SourceKind,
    pub interval: Interval,         // from resolve_valid_from / resolve_valid_until
    /// `Ongoing` opens (or confirms) an interval; `Ended` closes the matching
    /// open ones. Set from `ExtractedFact.state` OR from `cessation_of` having
    /// matched the raw predicate — the two spellings of the same statement.
    pub state: ObservationState,    // Ongoing | Ended
    pub autonomy: Autonomy,
    pub queue_everything: bool,     // ENGRAM_FACT_QUEUE_ALL, read once by the applier
}

/// The row the applier will insert, if the plan opens an interval.
pub struct PlannedFact {
    pub id: Uuid, pub fact_type: FactType,
    pub interval: Interval,
    /// Pre-closed on arrival: an `Ended` observation with nothing open to close
    /// (see PlanNote::EndWithoutOpenInterval).
    pub close: Option<(CloseReason, CloseAuthority)>,
}

/// Non-fatal reports that reach the envelope. Each renders to the same JSON
/// shape as today's `date_warnings` entries (`{kind, subject, predicate, detail}`)
/// and lands in a new `plan_notes` array alongside them — additive.
pub enum PlanNote {
    UnreadableValidUntil { raw: String },
    ValidUntilBeforeValidFrom { raw: String },
    EndWithoutOpenInterval { member: String },
    ReplayApplied { key: String, verdict: DecisionVerdict },
}

pub struct WritePlan {
    /// The interval to open, if any. `None` = the observation was absorbed.
    pub insert: Option<PlannedFact>,
    /// Facts to bump a confirmation on.
    pub confirm: Vec<Uuid>,
    /// Intervals to close, each with the authority that entitles the close.
    pub closes: Vec<PlannedClose>,
    /// Pairs the caller must decide, already carrying their semantic key.
    pub queue: Vec<PlannedConflict>,
    /// Prior decisions that were applied instead of asking again.
    pub replays: Vec<PlannedReplay>,
    /// Non-fatal reports for the envelope (unreadable dates, an end with no
    /// open interval to close).
    pub notes: Vec<PlanNote>,
}

pub struct PlannedClose {
    pub fact_id: Uuid, pub at: DateTime<Utc>,
    pub reason: CloseReason, pub authority: CloseAuthority,
    pub closed_by: Option<Uuid>, pub resolution_tag: &'static str,
}

pub struct PlannedConflict {
    pub new_fact_id: Uuid, pub old_fact_id: Uuid,
    pub detector: &'static str, pub score: Option<f32>,
    pub verdict: Verdict, pub key: SemanticKey,
    pub suggested: &'static str, pub allowed: &'static [&'static str],
    pub member_key: String, pub registry_version: &'static str,
}

/// `siblings` = every fact (open or closed) in (kb, diary) with the same
/// subject_norm and canonical relation. `others` = novelty-gate candidates
/// from other relations, already scored.
pub fn plan_write(
    observation: &Observation,
    siblings: &[Fact],
    others: &[(Fact, f32)],
    mode: ClassifyMode,
    prior: &dyn Fn(&SemanticKey) -> Result<Option<DecisionRecord>, StorageError>,
) -> Result<WritePlan, StorageError>;
```

`prior` is the only thing in the planner that touches a store, and it returns `Result` on
purpose: a failed decision lookup aborts the write rather than being read as "nobody has decided
this" (§2.3.2). Unit tests pass a closure over a fixture map and never construct a store.

**`Autonomy::Strict` and `ENGRAM_FACT_QUEUE_ALL` move INTO the planner.** Today they live in
`apply_write_verdict` (`fact_write.rs:1123-1134`): `Strict` escalates `SupersedeOld`/`SupersedeNew`
to `Queue`, `queue_everything()` forces `Queue` for everything except `Duplicate`, and
`cli.rs::strict_autonomy_queues_even_plain_slot_updates` pins the first. Leaving them in the
applier would put two policies in two places and let a plan assert a close that the applier then
refuses. So `plan_write` applies them as step 9.5, after `safety_gate`:

- `observation.autonomy == Strict` → every `closes` entry whose authority is `SuccessorInterval`
  or `Heuristic` becomes a `queue` entry instead. `ExplicitEnd` and replayed caller decisions are
  NOT escalated: `strict` promised never to auto-supersede, not to disbelieve the text or
  re-ask a question the caller already answered.
- `observation.queue_everything` → every `closes` entry becomes a `queue` entry, and `confirm`
  stays (a confirmation is not a decision). This is a test seam, and it keeps its exact
  present-day reach.

Both are read once by the applier and passed in on the `Observation`, so the planner stays pure.

### 2.4 `crates/engram-cli/src/fact_write.rs` — the applier

`write_fact` (today `fact_write.rs:858-1097`) becomes a thin wrapper: resolve the observation,
load siblings, call `plan_write`, apply. New order of operations:

1. `WRITE_FAIL_ENV` seam — unchanged (`:865-875`).
2. `relation = canonical_relation(&ef.predicate)`; if `cessation_of` matched, the observation's
   state becomes `Ended`.
3. `canonicalize_subject` — unchanged (`:515-557`).
4. `spec = relation_spec(&relation)`; `fact_type` is `ProfileSlot` when `spec.is_single_valued()`,
   otherwise the extractor's type, otherwise `Assertion` (replaces the `slot_policy(...).is_some()`
   test at `:885-890`).
5. `resolve_valid_from` — unchanged (`:485-510`), plus a new `resolve_valid_until` with the same
   discipline: an unreadable `valid_until` produces a `DateWarning` and is dropped, never
   silently set to now; a `valid_until` earlier than `valid_from` is dropped with a warning.
6. Load siblings **once**: `store.facts_for_relation(kb, diary, subject_norm, relation, include_closed = true)`.
   Only same-relation rows are considered for interval work. Pairs against a different member of
   a set relation are never constructed, so the write cost is O(same member), not O(all values):
   the fifth medication compares against zero facts, not four.
7. `plan_write` decides. For an `Ended` observation the plan closes every open interval whose
   member matches (authority `ExplicitEnd`, reason `Stopped`); if nothing is open it inserts a
   pre-closed interval and records a `PlanNote::EndWithoutOpenInterval` so the caller learns the
   store had no record to close.
8. The novelty-gate path (`:953-1095`) is unchanged in shape — same thresholds, same
   `MAX_CANDIDATES` — but its candidates are filtered through `classify_views` and then through
   replay before anything is queued, and `--autonomy full` LLM adjudication (`:1022-1071`) can no
   longer close a safety interval: `AdjudicationAction::Supersede` on a safety relation is
   downgraded to a queued pair. An LLM is a `Heuristic` authority.
9. Apply: insert, confirm, close, queue, in that order, inside the caller's transaction
   (`write_fact_batch:795-856` is otherwise untouched — batch atomicity, the pre-warmed embed
   cache, and the marker rule all stand).

`queue_conflict` (`:1197-1239`) changes shape:

```rust
fn queue_conflict(ctx: &AppContext, p: &PlannedConflict, ...) -> Result<(), CliError>
```

- `suggested` comes from `suggested_action`, never from `detector == "slot"` (kills `:1209-1213`);
- the row carries `decision_key`, `member_key` and `registry_version`;
- the payload gains `resolve: { program: "engram", args: [...], allowed_actions: [...] }` and
  `resolve_command` becomes ONE runnable command with no `|` (interfaces.md §4):
  `engram facts resolve <id> --action keep-both`;
- `note` explains coexistence honestly: "both values stay active until resolved" is only true for
  values that are genuinely both open, which after this change they always are.
- **a key collision is reported, not swallowed.** `insert_fact_conflict` is `INSERT OR IGNORE`
  (`sqlite.rs:4240`) and returns `Ok(false)` when nothing was inserted. With `idx_fcq_open_key`
  in place, a second distinct pair carrying an already-open semantic key hits that branch — and
  today `queue_conflict` simply pushes nothing onto `outcome.queued`, so the caller gets an
  exit-0 success with the question missing. Instead: on `Ok(false)`, look up the open row for
  `(kb, decision_key)` and push a `queued` entry naming THAT `conflict_id`, with
  `"deduplicated": true`. The agent is told which question is already waiting rather than being
  told nothing happened.

`apply_write_verdict` (`:1103-1163`) is replaced by `apply_plan`. `write_supersede_rule`
(`:311-318`), `classify_mode_for` (`:320-336`) and `compile_resolution` (`:338-354`) move to
`semantics.rs` unchanged — a compiled fact is still read like a backlog, not like an assertion.

### 2.5 `crates/engram-graph/src/facts.rs` — extraction

`ExtractedFact` (`facts.rs:61-74`) gains two `#[serde(default)]` fields and a `Default` impl:

```rust
pub struct ExtractedFact {
    // ... unchanged: subject, predicate, object, confidence, fact_type, valid_from
    /// ISO date when the fact STOPPED being true, if the text says so.
    #[serde(default)] pub valid_until: Option<String>,
    /// "ongoing" (default) | "ended" — an explicit cessation closes the open
    /// interval instead of opening a second one.
    #[serde(default)] pub state: Option<String>,
}
```

`FACT_EXTRACTION_PROMPT` (`facts.rs:30-59`) gains, in the same register as the existing rules:

- `valid_until`: absolute ISO date, resolved against the DATE CONTEXT, or null. Same warning as
  `valid_from`: a date nobody can parse is thrown away downstream.
- `state`: `"ended"` when the text says the state STOPPED ("stopped taking X", "no longer works
  at Y", "moved out of Z in June"); `"ongoing"` otherwise.
- one worked example with a cessation, so the model has a shape to copy.
- an instruction that a second medication/allergy/diagnosis is an ADDITIONAL fact, never a
  replacement — the extractor must not "helpfully" drop the earlier one.

`ADJUDICATION_PROMPT` (`facts.rs:270-280`) gains one line: a set-valued attribute (medications,
allergies, diagnoses, relationships) coexists unless the text says one ENDED; only a single-valued
attribute supersedes.

`slot_policy` / `SlotPolicy` (`facts.rs:225-246`) stay, marked `#[deprecated(note = "use
relations::relation_spec")]`, delegating to the registry so no external caller breaks:
`SingleValued{safety:true}` → `AwaitConfirmation`, `SingleValued{safety:false}` →
`LastWriterWins`, set-valued → `None`. `date_valued_predicate` (`:253-266`) is kept and
re-expressed as `relation_spec(c).value == ValueDomain::Date`.

### 2.6 SQL the storage lane must land (this lane's requirements)

```sql
-- interfaces.md §3 already pins these two:
ALTER TABLE facts ADD COLUMN closed_by     TEXT;   -- fact uuid (successor) or event uuid
ALTER TABLE facts ADD COLUMN close_reason  TEXT;   -- superseded|stopped|corrected|retracted_source
-- this lane additionally needs, for auditability of the safety gate:
ALTER TABLE facts ADD COLUMN close_authority TEXT; -- explicit_end|caller|replayed_caller|successor|heuristic

CREATE INDEX IF NOT EXISTS idx_facts_open_interval
    ON facts(kb, diary, subject_norm, predicate)
    WHERE valid_until IS NULL AND superseded_by IS NULL;
CREATE INDEX IF NOT EXISTS idx_facts_relation
    ON facts(kb, diary, subject_norm, predicate);

ALTER TABLE fact_conflict_queue ADD COLUMN decision_key     TEXT;
ALTER TABLE fact_conflict_queue ADD COLUMN member_key       TEXT;
ALTER TABLE fact_conflict_queue ADD COLUMN registry_version TEXT;
-- one open question per semantic key per KB, whatever fact ids it wears today.
-- Created LAST, after §4.1 step 5.2 has collapsed pre-existing duplicate keys:
-- a UNIQUE index rejects duplicates, it does not merge them.
CREATE UNIQUE INDEX IF NOT EXISTS idx_fcq_open_key
    ON fact_conflict_queue(kb, decision_key) WHERE status = 'open' AND decision_key IS NOT NULL;

-- projection of decision.recorded (interfaces.md §2), read by replay
CREATE TABLE IF NOT EXISTS fact_decisions (
    id           TEXT PRIMARY KEY,
    kb           TEXT NOT NULL DEFAULT 'default',
    diary        TEXT NOT NULL DEFAULT 'default',
    decision_key TEXT NOT NULL,
    subject_norm TEXT NOT NULL,
    relation     TEXT NOT NULL,
    pair_hash    TEXT NOT NULL,
    verdict      TEXT NOT NULL,   -- keep_both|close_old|reject_new|void
    authority    TEXT NOT NULL,   -- caller|machine
    actor        TEXT NOT NULL,
    note         TEXT,
    conflict_id  TEXT,
    event_id     TEXT NOT NULL,
    key_version  TEXT NOT NULL,
    decided_at   TEXT NOT NULL
);
-- `authority` is in the index because the precedence rule (§2.3.2) is
-- "newest CALLER row, else newest machine row", not "newest row".
CREATE INDEX IF NOT EXISTS idx_fact_decisions_key
    ON fact_decisions(kb, decision_key, authority, decided_at DESC);
```

`fact_conflict_queue.status` gains one value, `superseded_question`, used only by §4.1 step 5.2
and by the write path when a new pair collides with an open question that already carries its
key. Read-surfaces filters on `status = 'open'` throughout, so the new value is invisible to
every existing listing; it is not `resolved`, because nobody resolved it.

---

## 3. File-by-file change plan

| File | Change | Anchors |
|---|---|---|
| `crates/engram-graph/src/relations.rs` | **NEW** ~420 lines. `RelationSpec`, registry, `relation_spec`, `relation_arity`, `canonical_relation`, `cessation_of`, `member_identity`, `predicate_arity` + `LegacyArity` (legacy projection), `is_safety_slot`, `REGISTRY_VERSION`, `MEMBER_IDENTITY_VERSION`, unit tests. | absorbs `fact_write.rs:89-151` |
| `crates/engram-graph/src/interval.rs` | **NEW** ~280 lines. `Interval`, `is_open_fact`, `CloseReason`, `CloseAuthority`, `CloseVerdict`, `may_close`, `close_at`, unit tests. | new |
| `crates/engram-graph/src/semantics.rs` | **NEW** ~700 lines. `ConflictClass`, `Verdict`, `ClassifyMode`, `FactView`, `classify_pair`, `classify_views`, `safety_gate`, `suggested_action`, `allowed_actions`, `write_supersede_rule`, `classify_mode_for`, `compile_resolution`, `SemanticKey`, `DecisionVerdict`, `DecisionAuthority`, `DecisionRecord`, `DecisionLedger`, `replay`, `Observation`, `WritePlan`, `plan_write`, unit tests. | moves `fact_write.rs:73-354`, rewrites `:228-309` |
| `crates/engram-graph/src/facts.rs` | EDIT. `ExtractedFact` +`valid_until` +`state` +`Default`; prompt gains `valid_until`/`state`/coexistence rules + a cessation example; adjudication prompt gains the set-valued line; `slot_policy` deprecated shim; `date_valued_predicate` re-expressed. | `:30-59`, `:61-74`, `:225-266`, `:270-280` |
| `crates/engram-graph/src/lib.rs` | EDIT. `pub mod interval; pub mod relations; pub mod semantics;` + re-export `relation_spec`, `member_identity`, `classify_pair`, `SemanticKey`. | `:11-22` |
| `crates/engram-graph/Cargo.toml` | EDIT. add `blake3.workspace = true` (see §6.1). | |
| `crates/engram-cli/src/fact_write.rs` | EDIT, net −350 lines. Delete the classifier and arity tables; re-export the moved names — including `LegacyArity as Arity` and `is_safety_slot` — so `commands/facts.rs:20-24` is untouched; rewrite `write_fact` around `plan_write` + `apply_plan`; new `resolve_valid_until`; `queue_conflict` takes a `PlannedConflict`; `--autonomy full` supersession is safety-gated; module header rewritten to describe intervals rather than slots. | `:1-30`, `:73-354`, `:858-1097`, `:1099-1163`, `:1197-1239` |
| `crates/engram-cli/src/fact_llm.rs` | EDIT (small). The stub-script docs mention the two new `ExtractedFact` fields so a test author knows the seam covers cessation. No behaviour change. | `:1-25` |
| `crates/engram-cli/tests/wave3_fact_intervals.rs` | **NEW** — see §5.E. | |
| `crates/engram-cli/tests/wave3_fact_polypharmacy.rs` | **NEW** | |
| `crates/engram-cli/tests/wave3_fact_replay.rs` | **NEW** | |
| `crates/engram-cli/tests/wave3_fact_safety_gate.rs` | **NEW** | |

Tests inside `fact_write.rs:1241-1638` move with the code they cover. Three of them change
meaning and the commit message must say so:

- `arity_defaults_to_multi_and_knows_the_slots:1395-1407` — `predicate_arity("allergic_to")`
  becomes `Multi`. That assertion IS the polypharmacy defect, written down as a test.
- `safety_slots_are_never_auto_resolved_for_one_subject:1468-1486` — splits into "different
  medications coexist" and "a dose change on one medication queues", both preserving the
  never-auto-superseded guarantee.
- `different_predicates_on_one_subject_coexist_unless_they_are_one_slot:1432-1456` — the
  `lives_in` vs `born_on` paraphrase case survives; `born_on` is now `Immutable`, so the
  same-relation birthday case moves to its own test.

---

## 4. Migration and compatibility

### 4.1 Conversion of the existing store

The audit measured **951 open** and **~14.5k resolved** `fact_conflict_queue` rows. Those
numbers are quoted from the brief, not re-measured — hard constraint 1 forbids opening the live
store, and this spec does not do so.

**Phase 0 — measurement (manual, operator-run, once).** The only sanctioned way to see the real
numbers, and every clause of it is load-bearing:

1. Stop any daemon.
2. Take the snapshot with SQLite, not with `cp`:
   `sqlite3 ~/Library/Application\ Support/bio.199-biotechnologies.engram/db.sqlite "VACUUM INTO '<tmp>/db.sqlite'"`.
   `VACUUM INTO` is a consistent read against a live WAL; copying `db.sqlite` + `-wal` + `-shm`
   by hand races any process that still holds the file and can produce a torn store, which is
   the wrong thing to be measuring.
3. `export ENGRAM_STORE=<tmp>` and run the v0.4 binary from `target/debug` (constraint 2: never
   `cargo install`). **`ENGRAM_STORE` only — never `HOME`.** Without `ENGRAM_STORE` the binary
   resolves through `ProjectDirs`, so a bare `HOME` override points at
   `<tmp>/Library/Application Support/bio.199-biotechnologies.engram/` and, if that path does not
   exist, the store is silently created empty (or, with the wrong `HOME`, is the real one).
   interfaces.md §5 says this in one line: never rely on HOME tricks again.
4. `engram facts triage --dry-run --json`. Record `resolved_by_rule` and `remaining_by_reason`.

The v0.4 binary WILL run the v9→v10 migration against the snapshot before answering — that is
fine and intended (it is a copy), but it means the numbers are post-migration numbers and the
snapshot is not reusable for a second before/after comparison. Take a fresh one for Phase 3.
Never automate this, never put it in a test, never point a binary at the real path.

**Phase 1 — migration v9→v10 (storage-journal lane, calling this lane's pure functions).**
Ordered, idempotent, backup first. The backup filename is `db.sqlite.pre-v10-backup`, exactly as
interfaces.md §2 spells it; the brief's constraint 4 asks for a *timestamped* backup, and the two
documents disagree. This lane does not own the migration and does not get to pick — §6.8 asks the
lead to pin one spelling, and until then the implementer follows interfaces.md.

The steps are ordered, and the order matters more than it looks — step 1 creates a UNIQUE index
over a column step 5 populates:

1. Add the columns of §2.6 and the two non-unique `facts` indexes. **Do NOT create
   `idx_fcq_open_key` here** — see step 5.
2. **Intervals.** For every `facts` row:
   - `valid_from = COALESCE(valid_from, created_at)` — after this, `valid_from` is never NULL;
   - rows with `superseded_by NOT NULL`:
     `valid_until = MAX(valid_from, COALESCE(valid_until, superseded_at, created_at))`,
     — the `MAX` is `close_at`'s clamp (§2.2) expressed in SQL, and it is not optional: a fact
     whose stated `valid_from` is later than its write time (a future-dated `lives_in`, an
     import) would otherwise be migrated to `valid_until < valid_from`, a negative-length
     interval that `overlaps` and every downstream consumer reads as nonsense;
     `closed_by = superseded_by`,
     `close_reason` mapped from `resolution`:
     `caller` → `corrected`; `duplicate` → `superseded`; `slot_lww`/`exact_match`/`semantic_llm`/
     `recency_*`/`triage_*`/`compile_*`/NULL → `superseded`;
     `close_authority` mapped: `caller` → `caller`; everything else → `heuristic`;
   - rows with `superseded_by NULL` keep `valid_until` NULL: they are the open intervals, and
     they are what `engram state` will read.
3. **Genesis events.** `fact.observed` per fact row; `fact.interval_closed` per closed row with
   the mapped reason. (Exact payloads per interfaces.md §2.)
4. **Resolved queue rows → decision events (~14.5k).** Join each row to both facts. Infer:
   - `new.superseded_by == old.id` → `reject_new`;
   - `old.superseded_by == new.id` → `close_old`;
   - neither → `keep_both`.
   Authority: `resolved_by = 'caller'` → `caller`; `'triage'`, `'compile'`, NULL → `machine`.
   `decision_key = SemanticKey::for_pair(new, old).encode()`, `key_version = "semantic_key@1"`.
   Emit `conflict.opened` then `decision.recorded`; insert the `fact_decisions` projection row.
   Rows whose facts no longer exist (a `delete_facts_for_memory` cascade took them) are skipped
   and counted in the migration's summary — there is nothing left to key them by.
5. **Open queue rows (951), then the unique index.** In this order:
   1. Emit `conflict.opened` and backfill `decision_key`, `member_key`, `registry_version` on
      every open row.
   2. **Collapse duplicate keys explicitly.** A partial UNIQUE index does not merge duplicates —
      it rejects them, and the second `UPDATE` onto an existing key aborts the whole migration.
      Duplicate semantic keys among the open rows are not a corner case: they ARE the storm this
      lane exists to kill (the same pair queued again on every re-derivation). So, for each
      `(kb, decision_key)` group with more than one open row, the SURVIVOR is the row with the
      lowest `created_at` (ties broken by `id`) — the question as first asked, whose id may
      already be in an agent's notes or a `resolve_command` someone saved. Every other row in the
      group is set `status = 'superseded_question'` (a new terminal status, never `resolved`,
      because nobody answered it) with `resolved_by = 'migration'` and a
      `decision.recorded { verdict: "void", authority: "machine" }` naming the survivor.
      The count lands in the migration summary as `open_conflicts_collapsed`.
   3. Only now `CREATE UNIQUE INDEX idx_fcq_open_key`. Creating it earlier is a guaranteed
      migration abort on any store that has ever queued the same pair twice.
   Rows whose `decision_key` cannot be computed (a cascaded fact) keep `decision_key = NULL`; the
   index's `AND decision_key IS NOT NULL` clause is what lets them stay open and visible.
6. Idempotency guard: every step is `WHERE NOT EXISTS (SELECT 1 FROM events WHERE ...)` or an
   `INSERT OR IGNORE`; running the migration twice must produce zero additional events.
   Step 5.3 is `CREATE UNIQUE INDEX IF NOT EXISTS`, and step 5.2 is a no-op on a second run
   because the collapsed rows are no longer `status = 'open'`.

**Phase 2 — drain (user-run, after upgrading; writes nothing during migration).**
`engram facts triage` re-classifies the 951 under the new rules. Expected classes, in the order
they will dominate:

| class | rule | disposition |
|---|---|---|
| different medications / allergies / diagnoses on one person | `safety_set_coexist` | resolved, both intervals stay open, nothing superseded |
| pairs whose loser was already retired | `already_settled` | resolved, touches nothing |
| single-valued values that never overlapped in time | `disjoint_intervals` | resolved, both stay queryable, current state = the open interval |
| second value of a multi-valued relation | `multi_valued` | resolved (already true in v0.3.1) |
| same medication, different dose | `safety_member_change` → gate → `safety_slot` | **stays queued**, `suggested_action: requires_context` |
| two employers from one memory | `same_source_contradiction` | stays queued, `requires_context` |
| two birthdays | `immutable_attribute_disagreement` | stays queued, `requires_context` |

`disjoint_intervals` is listed third, but do not budget on it. Two rows that are both OPEN both
run to infinity, so they overlap by definition and the rule cannot fire — it only reaches pairs
where one side already has a `valid_until`, and in a v9 store the only thing that sets
`valid_until` is a supersession, which step 2 also maps to `superseded_by` and step 4 to
`already_settled`. The London/Lisbon story is what the rule buys GOING FORWARD, once the
extractor emits `valid_until` (§2.5); its share of the existing 951 is probably near zero.
Phase 0 measures the real distribution, and the ordering of this table is a hypothesis, not a
result.

Every triage resolution appends `decision.recorded` with `authority = machine`, so the sweep is
auditable and replayable — and, per §2.3.2, only its `keep_both` verdicts are binding later.
Triage may not close a safety interval: the gate refuses `Heuristic`, and
`commands/facts.rs:581-593`'s defence-in-depth check is replaced by a call to `safety_gate`
so there is one implementation of that guarantee rather than two.

**Phase 3 — verification.** On the Phase 0 copy: `facts conflicts --open --json` count before
and after; `facts list --subject <person> --json` shows every medication as an open interval with
`valid_until: null`; no fact acquires `close_authority = 'heuristic'` on a safety relation
(a single SQL assertion, and the reason `close_authority` is worth a column).

**Rollback.** The pre-v10 backup file is the operable rollback and the only one to document for
users. `engram admin rebuild` (interfaces.md §2, invariant 3) is the *architectural* rollback —
the projections are rebuildable from the journal by construction — but no such command exists in
v0.3.1 and it is storage-journal's to ship. Until it lands, this lane's rollback story is
"restore the backup", and any release note that says otherwise is writing a cheque another lane
has not signed.

### 4.2 Compatibility

- **CLI surface**: unchanged. `facts resolve --action supersede-old|keep-both|reject-new` keeps
  all three actions and their meanings; `supersede-old` now closes an interval with
  `CloseAuthority::Caller`, which is what it already did (it set `valid_until`, `sqlite.rs:4105-4133`).
- **JSON fields**: `arity` keeps emitting `"single"|"multi"` from the legacy projection;
  `suggested_action` keeps its name and type, and gains one new value (`requires_context`) while
  losing one it emitted (`supersede-old`) — a value change, not a schema change, and the point of
  the lane. `resolve_command` stays, now pipe-free. `conflicts[]`, `adjudication_needed[]`,
  `resolved_by_rule`, `remaining_by_reason`, `safety_slots_kept` keep their shapes and keys.
- **Rule names**: existing names are preserved wherever the meaning is unchanged, including
  `safety_slot` for the gate output, because another lane reads it as a map key.
- **`fact_type`**: new safety-relation facts stop being classified `profile_slot` (they are
  set-valued now) and take the extractor's type or `assertion`. Existing rows are untouched, and
  nothing in the repo BRANCHES on `FactType::ProfileSlot` — the only assignment is
  `fact_write.rs:885-890` and the only other mentions are the extraction prompt and a storage
  test fixture (`sqlite.rs:5570`). But `fact_type` is a published JSON field, so anything
  outside this repo that filters allergies or medications by `fact_type == "profile_slot"` sees
  them disappear. That is a value change on a legacy field, which constraint 3 permits (names and
  types are preserved) and which belongs in the CHANGELOG next to the `suggested_action` change,
  not in a footnote.
- **v9 stores read by a v10 binary** never happen (migration runs first), but `Interval::of`
  tolerates NULL `valid_from` anyway rather than panicking on a hand-restored backup.
- **`bench personal --json`** must be byte-identical: it exercises recall, not adjudication.
  Gate it in the lane's test run.

---

## 5. Test plan

Hard constraint 1 governs everything below. **No test in this lane may open the live store.**
Two structural defences:

1. Most of this lane is pure functions over `Fact` fixtures, so most tests cannot reach a store
   at all. That is a design goal, not an accident: `plan_write` exists so the polypharmacy
   guarantee is provable without SQLite.
2. Every integration test builds its command through a per-file helper (repo precedent:
   `tests/wave05_fact_batch.rs:17-36`) that sets `HOME`, `XDG_CONFIG_HOME`, `XDG_DATA_HOME`,
   `XDG_CACHE_HOME` **and** `ENGRAM_STORE` to a fresh `TempDir`, sets
   `ENGRAM_BENCH_FORCE_STUB=1`, and `env_remove`s `GEMINI_API_KEY`, `COHERE_API_KEY`,
   `OPENROUTER_API_KEY`, `ENGRAM_FACT_LLM_STUB`, `ENGRAM_FACT_WRITE_FAIL`, `ENGRAM_FACT_QUEUE_ALL`.
   `Command::cargo_bin` appears in exactly one function per file; review check:
   `rg -n 'cargo_bin' crates/engram-cli/tests/wave3_fact_*.rs` must show one hit per file.

### A. `relations.rs` unit tests
1. `safety_relations_are_sets_not_slots` — `takes_medication`, `allergic_to`, `diagnosed_with`
   → `SetValued { safety: true }`. The single assertion that kills the storm.
2. `blood_type_is_a_single_safety_value`; `birthday`/`died_on`/`nationality` →
   `SingleValued { safety: false }` + `Immutable`.
3. `unknown_relations_stay_multi_valued` — `collaborated_with` → `SetValued{false}`; every entry
   of the old `MULTI_VALUED_PREDICATES` list still set-valued (pinned as a loop, as today).
4. `legacy_arity_projection_is_stable` — `predicate_arity("lives_in") == Single`,
   `predicate_arity("knows") == Multi`, `predicate_arity("allergic_to") == Multi` (**changed**).
5. `member_identity_strips_dose_route_and_frequency` — a table: `empagliflozin 12.5 mg daily`,
   `empagliflozin 25mg`, `Empagliflozin (25 mg, oral)` all → `empagliflozin`; `metformin` and
   `metformin xr` stay distinct; `type 2 diabetes` survives intact; `hypertension stage 2` →
   `hypertension`; `penicillin (severe)` → `penicillin`.
6. `member_identity_never_returns_empty` — property test over 1k generated objects of noise
   tokens only; the fallback is the full object.
7. `member_identity_is_pure_and_versioned` — same input twice, same output; version constant present.
8. `cessation_predicates_map_to_the_relation_they_end`.

### B. `interval.rs` unit tests
9. `overlap_is_half_open` — `[a,b)` vs `[b,c)` do not overlap; open ends overlap everything after.
10. `point_intervals_never_overlap`.
11. `close_at_never_precedes_valid_from`.
12. `may_close_matrix` — the full cross product of {safety set, safety single, routine single,
    immutable} × {ExplicitEnd, Caller, ReplayedCaller, SuccessorInterval, Heuristic}. Asserted as
    a table so a future edit that widens the gate fails loudly.

### C. `semantics.rs` unit tests (classification)
13. `five_medications_produce_no_conflicts` — the reproduced defect. Build 5 medication facts for
    one subject; assert every one of the 10 pairs classifies `Coexist`, and that `plan_write` for
    the fifth yields `queue.is_empty()` and `closes.is_empty()`.
14. `same_drug_new_dose_is_a_question_not_a_supersession` — `Queue`, rule `safety_slot` after the
    gate, `suggested_action == "requires_context"`, and the plan closes nothing.
15. `a_dose_change_on_a_routine_set_relation_closes_the_old_interval`.
16. `no_verdict_ever_closes_a_safety_interval` — property test: for the cross product of safety
    relations × object pairs × time orderings × source kinds × both `ClassifyMode`s, assert the
    class is never `SupersedeOld`/`SupersedeNew`.
17. `suggested_action_is_never_supersede_old` — property test over every `Verdict` the classifier
    can emit.
18. `disjoint_intervals_coexist` — London 2015–2020 vs Lisbon 2024–open → `Coexist`.
19. `overlapping_single_valued_intervals_close_the_older_one` — and the close instant equals the
    successor's `valid_from`.
20. `an_already_settled_safety_pair_is_settled_not_queued` — the 951-drain rule; in v0.3.1 this
    returns `Queue("safety_slot")`.
21. `two_birthdays_are_a_contradiction_no_clock_can_settle`; and
    `a_user_correction_supersedes_an_extracted_birthday`.
22. `a_reinstated_medication_is_a_second_interval_not_a_duplicate`; and its sibling
    `a_backdated_assertion_inside_a_closed_window_is_settled` — the arm that stops a
    stopped-and-restated value from being re-opened by a clock (§2.3.1 step 4).
22b. `a_safety_duplicate_is_queued_not_deduped` — the gate covers `Duplicate` because `Duplicate`
    supersedes the incoming row today (`facts.rs:635-643`, `fact_write.rs:1137-1146`), and
    `apply_plan` confirms instead of superseding. Asserts both: the class, and that the plan's
    `closes` is empty for a non-safety duplicate too.
22c. `strict_autonomy_escalates_heuristic_closes_but_not_explicit_ends`, and
    `queue_everything_forces_every_close_into_the_queue` — the two policies §2.3.3 moves out of
    `apply_write_verdict`, now provable without a store.
23. `different_subjects_never_conflict`, `duplicate_object`, `same_source_contradiction`,
    `comparable_values`, `confidence_gap`, `paraphrase_slot`, `compiled facts read as a backlog`
    — the surviving v0.3.1 tests, moved.
24. `classification_is_order_consistent` — for `Coexist`/`Duplicate`/`Settled`, `classify(a,b)`
    and `classify(b,a)` agree on the class.

### D. `semantics.rs` unit tests (keys and replay)
25. `semantic_key_is_pair_order_independent` — `for_pair(a,b) == for_pair(b,a)`.
26. `semantic_key_changes_with_subject_relation_or_either_object`.
27. `semantic_key_survives_re_derivation` — same triples, fresh UUIDs, fresh source memory → same key.
28. `a_caller_keep_both_suppresses_the_question`; `a_caller_close_old_reapplies_silently`; and
    `a_caller_decision_outranks_a_newer_machine_one` — the precedence rule of §2.3.2, which
    "most recent row wins" would get backwards.
29. `a_machine_close_old_never_replays` — the guard against re-freezing the old classifier.
30. `a_machine_keep_both_does_replay`.
31. `encode_decode_roundtrip`, including subjects containing the `\u{1f}` separator (rejected/escaped).

### E. Integration tests (temp store, scripted LLM, offline)
Each uses `ENGRAM_FACT_LLM_STUB` with a scripted JSON array, so no key and no network.

32. `wave3_fact_polypharmacy.rs::five_medications_queue_nothing` — five `remember` calls, each
    scripting one `takes_medication` triple; then `facts conflicts --open --json` →
    `count == 0` and `total == 0`; `facts list --subject "Ana" --json` → five facts, every one
    with `valid_until: null`. Run `facts triage --json` afterwards: `scanned == 0`.
33. `wave3_fact_intervals.rs::an_explicit_end_closes_the_interval` — script
    `{"predicate":"stopped_taking","object":"metformin","valid_until":"2026-06-01"}`;
    assert the earlier fact has `valid_until == 2026-06-01` and `close_reason == "stopped"`, and
    that no conflict was queued.
34. `wave3_fact_intervals.rs::restarting_a_medication_opens_a_second_interval` — assert two rows,
    one closed one open, and `facts list` (active only) returns one.
35. `wave3_fact_intervals.rs::an_end_with_no_open_interval_is_reported_not_invented` — the
    envelope carries the note; a closed interval is stored.
36. `wave3_fact_safety_gate.rs::a_dose_change_queues_with_requires_context` —
    `adjudication_needed[0].suggested_action == "requires_context"`,
    `resolve_command` contains no `|`, `resolve.allowed_actions` includes `keep-both`, and BOTH
    facts are still active.
37. `wave3_fact_safety_gate.rs::triage_never_closes_a_safety_interval` — build the pair, run
    `facts triage`, assert the conflict is still open, `safety_slots_kept == 1`, and both facts
    still have `valid_until: null`. Then `facts triage --dry-run` reports identical numbers
    (the honesty property `commands/facts.rs:519-535` already promises).
38. `wave3_fact_replay.rs::a_resolved_question_is_never_asked_twice` — remember A, remember B
    (queued), `facts resolve <id> --action keep-both`, then re-run `facts backfill` with the same
    stub script over an edited memory: no new open conflict, and the response reports the replay.
39. `wave3_fact_replay.rs::a_caller_supersede_replays_on_re_derivation` — same shape with
    `--action supersede-old`; after re-derivation the re-created old value is closed again with
    `close_authority == "replayed_caller"` and no queue row.
40. Regression, unchanged and must stay green: `wave05_fact_batch.rs` (batch atomicity, the
    marker rule, `caller_facts_do_not_hide_a_memory_from_extraction:209-234`),
    `cli.rs::date_valued_predicate_object_becomes_the_fact_event_time:2426`,
    `cli.rs::strict_autonomy_queues_even_plain_slot_updates` (the `Autonomy::Strict` escalation
    the planner absorbs, §2.3.3), `cli.rs:1959`/`:4474` (the `facts resolve` action surface).

41. **Regressions that MUST change, and whose diff is the deliverable.** Two shipped tests
    encode the polypharmacy defect as a guarantee, so "leave them green" and "fix the storm" are
    the same sentence read two ways. Both assert that two DIFFERENT allergies on one person
    produce exactly one queued `safety_slot` conflict:
    - `cli.rs:1791 safety_slot_queues_conflict_and_recall_reports_contested` — `Ana allergic_to
      penicillin` then `Ana allergic_to amoxicillin` → `adjudication_needed.len() == 1`. Under
      this spec that is `safety_set_coexist`: zero conflicts, two open intervals. The test is
      rewritten to assert coexistence, and a NEW sibling test keeps the half that is still true
      — a DOSE change on one medication queues, `recall` reports the subject contested.
    - `wave05_compiled_facts.rs:170 a_compiled_fact_never_retires_a_safety_slot` — expects
      `llm_facts_queued == 1` and `triage.rule == "safety_slot"`. Rewritten the same way: the
      claim worth keeping is `historic_count == 0` (a compile never retires a safety fact),
      which stays green; the queued count becomes 0 and the rule assertion moves to a
      dose-change fixture.
    Both rewrites go in their own commit, ahead of the behaviour change, with a message that
    says the assertion WAS the bug. Nothing else in `wave05_compiled_facts.rs` moves.

42. **Tests that depend on other lanes, and what they assert until those lanes land.**
    Marked `#[ignore]` with the blocking lane named in the attribute, so the gate is honest
    rather than green-by-omission:
    - test 34 (`facts list` hides a stopped medication) needs the `is_open_fact()` sweep of
      risk 5 — every "active" query is `superseded_by IS NULL` today
      (`sqlite.rs:3974-4030`, `:4150-4173`, `:4542-4760`), and a `close_reason = 'stopped'` row
      leaves `superseded_by` NULL. Until read-surfaces/storage land the sweep, the test asserts
      the STORED shape (`valid_until` set, `close_reason = "stopped"`) and leaves the listing
      assertion ignored. This lane's unit tests cover the semantics; only the listing is blocked.
    - tests 38–39 (replay across a re-derivation) need storage-journal: today `edit` calls
      `delete_facts_for_memory` (`sqlite.rs:1925`) and `fact_conflict_queue` cascades
      (`sqlite.rs:1107-1108`), so a decision does not survive the edit that would replay it —
      the brief's move-1 defect. Until the journal lands, replay is proved by unit tests 25–31
      and by an integration test that re-runs `facts backfill` over an UNEDITED memory, which
      exercises the same key path without needing the edit to be non-destructive.

### F. Gates before commit (constraint 5)
`cargo build --workspace`, `cargo clippy --workspace -- -D warnings`, `cargo fmt --check`,
`cargo test --workspace`, and `engram bench personal --json` in a temp store — unchanged output.

---

## 6. Interface needs

Everything below is a request to the integration lead for `docs/arch/interfaces.md`. Nothing here
is implemented divergently in the meantime; where a signature is missing this lane codes against
the shape written here and reports it.

### 6.1 `blake3` as a workspace dependency
interfaces.md §3 pins `object_pair_hash = hex(blake3(...))`. `blake3` is not in
`Cargo.toml`'s `[workspace.dependencies]`. **Request**: add `blake3 = "1"` and
`blake3.workspace = true` to `crates/engram-graph/Cargo.toml`.
*Alternative if a new dependency is unwanted*: `uuid` is already vendored with the `v5` feature,
so `Uuid::new_v5(&DECISION_NS, bytes).simple()` is a deterministic, dependency-free hash of the
same pair encoding. This lane will use blake3 as specified unless the lead says otherwise; the
choice must be made once, because it is baked into 14.5k migrated decision keys.

### 6.2 Storage methods this lane calls
```rust
impl SqliteStore {
    /// Every fact (open or closed, superseded or not) for one subject+relation
    /// in (kb, diary), newest first. Replaces per-write use of
    /// `get_active_facts_in_kb` (sqlite.rs:4002-4030), which cannot see closed
    /// intervals and therefore cannot tell a reinstatement from a duplicate.
    pub fn facts_for_relation(&self, kb: &str, diary: &str, subject_norm: &str, relation: &str,
                              include_closed: bool) -> Result<Vec<Fact>, StorageError>;

    /// Open intervals only — the projection `engram state` and recall read.
    pub fn open_intervals(&self, kb: &str, diary: &str, subject_norm: &str,
                          relation: Option<&str>) -> Result<Vec<Fact>, StorageError>;

    /// Close an interval. Sets valid_until, close_reason, close_authority,
    /// closed_by; appends `fact.interval_closed`. Does NOT set superseded_by
    /// unless reason == Superseded, which keeps the legacy readers correct
    /// (interfaces.md §3 compatibility view).
    pub fn close_fact_interval(&self, fact_id: Uuid, at: DateTime<Utc>, reason: &str,
                               authority: &str, closed_by: Option<Uuid>, actor: &str)
        -> Result<bool, StorageError>;

    /// Kept, and re-expressed as close_fact_interval(reason="superseded",
    /// authority="heuristic", closed_by=new_id) so the ~5 existing call sites
    /// (commands/facts.rs:480, :492, :630; fact_write) need no edit and every
    /// close lands in the journal.
    pub fn supersede_fact_with(&self, old: Uuid, new: Uuid, resolution: &str,
                               valid_until: Option<DateTime<Utc>>) -> Result<bool, StorageError>;

    /// Decision ledger.
    pub fn record_decision(&self, kb: &str, diary: &str, key_encoded: &str, verdict: &str,
                           authority: &str, actor: &str, note: Option<&str>,
                           conflict_id: Option<Uuid>, key_version: &str)
        -> Result<Uuid, StorageError>;
    pub fn lookup_decision(&self, kb: &str, key_encoded: &str)
        -> Result<Option<DecisionRow>, StorageError>;

    /// Queue insert gains the semantic key and provenance of the classification.
    pub fn insert_fact_conflict(&self, id: Uuid, new_fact_id: Uuid, old_fact_id: Uuid,
                                diary: &str, kb: &str, detector: &str, score: Option<f32>,
                                suggested: &str, decision_key: &str, member_key: &str,
                                registry_version: &str) -> Result<bool, StorageError>;
}
```
`insert_fact_conflict` (sqlite.rs:4229-4258) has exactly one caller
(`fact_write.rs:1214`), so widening it in place costs no other lane an edit.

### 6.3 `engram-core` additions
- `pub enum Arity { SetValued { safety: bool }, SingleValued { safety: bool } }` — already in
  interfaces.md §3; this lane needs it to land in `engram-core` (wave 2) so `relations.rs` can
  re-export rather than define a rival.
- `Fact` gains `close_reason: Option<String>`, `close_authority: Option<String>`,
  `closed_by: Option<Uuid>` (`#[serde(default)]`, additive). This lane only reads them.
- **Addition request**: `Mutability` and `Temporality` are NOT in interfaces.md. They are
  additive axes this lane needs (birthday vs lives_in behave differently under identical arity)
  and they live in `engram-graph::relations`, not in the pinned `Arity` enum, so nothing pinned
  diverges. Requesting they be recorded in §3 as fact-semantics-owned.

### 6.4 Read-surfaces (`commands/facts.rs`, `commands/recall.rs`)
1. Replace the inline safety defence at `facts.rs:581-593` with
   `engram_graph::semantics::safety_gate(verdict, &new_view, &old_view)` — one implementation of
   the guarantee instead of two that can drift.
2. `facts resolve` (`:451-517`) must append `decision.recorded` with
   `SemanticKey::for_pair(...).encode()` and `authority = caller`. Without this, replay has
   nothing to replay and move 3 is half-built.
3. `group_conflicts` (`:406-447`) keeps `arity: "single"|"multi"` and ADDS
   `arity_class: "set_valued"|"single_valued"` and `safety: bool` from `relation_spec`.
4. `resolve_command` at `facts.rs:372` and `recall.rs:346` must become pipe-free single commands
   with a `resolve.allowed_actions` array (interfaces.md §4); this lane already emits that shape
   from `queue_conflict`.
5. Optional, recommended: a `--action close-old --as-of <date>` variant so a caller can close an
   interval at a stated date rather than at the successor's start.

### 6.5 Daemon-jobs (`commands/compile.rs`)
`ExtractedFact` gains two `#[serde(default)]` fields, so the single struct literal at
`compile.rs:866-877` needs `..Default::default()`. One line, mechanical, pre-agreed here so the
workspace stays green whichever lane lands first.

### 6.6 Divergences from interfaces.md this lane is asking for

§1 promises every divergence is listed here rather than invented, and the earlier draft invented
five. Each of these is additive — nothing pinned in interfaces.md changes meaning — but each
needs the lead to record it:

1. **`facts.close_authority`** (§2.6). Not in interfaces.md §3, which pins only `closed_by` and
   `close_reason`. It is what makes the safety gate auditable after the fact: the Phase 3
   assertion "no safety fact was ever closed by a heuristic" is a one-line SQL query against this
   column and is not answerable without it.
2. **The `fact_decisions` projection table** (§2.6). interfaces.md §2 defines the
   `decision.recorded` EVENT but no projection to read it from, and replay must not scan the
   journal on every write.
3. **`authority` on the `decision.recorded` payload.** interfaces.md §2 lists
   `(decision_id, semantic_key, verdict, conflict_id?, note?, actor)`. This lane needs
   `authority: "caller"|"machine"` as a first-class field: `actor` is a free-text string
   (`"cli:<diary>"`, `"job:<uuid>"`) and the caller/machine distinction decides whether a
   decision replays at all (§2.3.2). Parsing it back out of `actor` would make the safety
   property depend on a string convention.
4. **`conflict.opened` for OPEN queue rows during migration.** interfaces.md §2's migration note
   synthesizes `conflict.opened` + `decision.recorded` from RESOLVED queue rows only. The 951
   open rows need `conflict.opened` too, or the journal cannot rebuild the open queue and
   invariant 3 (byte-identical rebuild) fails on any store with a backlog.
5. **`decision.replayed` is appended by the applier, not just planned.** interfaces.md §3 says
   replay "applies silently and appends `decision.replayed`". `apply_plan` (§2.4 step 9) appends
   one event per `WritePlan.replays` entry, in the same transaction as the effect, per
   invariant 1. Stated here because the earlier draft described the plan and never the append.
6. **`fact_conflict_queue.status = 'superseded_question'`** (§2.6, §4.1 step 5.2).

### 6.7 Write-surfaces — source kinds

§2.3.1 step 7's `correction_by_user` rule reads `SourceKind::UserStated | UserConfirmed`, and no
production path produces either: `remember.rs:632` stamps `AgentExtracted` unconditionally,
including for `--facts-json`, and only `compile` sets `CompileLlm`. **Request to write-surfaces**:
a fact asserted through an explicit `--facts-json` payload (the caller stating a triple, not an
extractor guessing one) should be `SourceKind::UserStated`. Without it the correction rule is
dead code that this lane's unit tests exercise and no user ever reaches. This lane does not own
`remember.rs` and does not implement it; if write-surfaces declines, the rule still costs nothing
and becomes reachable the day source kinds are honest.

### 6.8 Compile order, and where these methods live

Everything in §6.2 is a REQUEST to storage-journal, implemented in `crates/engram-storage`. The
one thing this lane implements against `SqliteStore` is `impl DecisionLedger for SqliteStore`,
which lives in `engram-graph` — legal (local trait, foreign type), and `engram-graph` already
depends on `engram-storage`, so the dependency direction is unchanged. Nothing else in §6.2 may
be written by this lane, however convenient: `engram-storage/**` is storage-journal's exclusive
file set (brief, lane table).

Wave sequencing says storage-journal lands first, so in the normal case the methods exist before
this lane compiles. If they do not, this lane does NOT stub them in `engram-graph` — it blocks and
says so in its report. A stub that silently returns `Ok(None)` from `binding_decision` is a
classifier that has forgotten every decision the user ever made, and it would pass every test in
§5.D because those use a fixture closure.

Interfaces.md §2 spells the migration backup `db.sqlite.pre-v10-backup`; the brief's constraint 4
requires a *timestamped* backup and cites `db.sqlite.pre-v8-backup` as precedent (which is not
timestamped either). **Request**: pin one spelling in interfaces.md so this lane's Phase 3
verification script and storage-journal's migration agree on the filename.

### 6.9 Open interface question — decision scope
`SemanticKey` (interfaces.md §3) carries no `kb`. A decision made in one KB would replay into
another, which is wrong for the same reason contradiction detection is KB-scoped
(`sqlite.rs:3996-4001`). **Request**: keep the key as specified, and scope the LOOKUP by `kb`
(`lookup_decision(kb, key)`, as written in §6.2). Diary is deliberately NOT in the lookup: diary
is an authorship channel, not a namespace, and a person's medication decision should not need
re-answering per diary.

---

## 7. Risks and open questions

1. **`member_identity` is a heuristic, and it is load-bearing for safety.** An over-merge
   (`vitamin d 1000` and `vitamin d 2000` collapsing to `vitamin d`) turns two true intervals
   into a queued dose-change question — noisy but safe. An under-merge (`metformin` vs
   `metformin xr`) leaves two open intervals — also safe. The unsafe direction is an over-merge on
   a NON-safety set relation, where the plan closes an interval automatically. Mitigations: the
   v1 rules only strip from the END and only from a closed vocabulary; the version is recorded on
   every queue row; tests 5–7 pin the table. Open question for the lead: should non-safety set
   relations auto-close on a member-attribute change at all, or should v1 queue those too and
   revisit after the drain is measured? This spec chooses auto-close, on the grounds that
   `has_role: "engineer"` → `"senior engineer"` should not need a human.
2. **Set relations never contradict, so a mis-extracted allergy is sticky.** "She is allergic to
   penicillin — sorry, amoxicillin" yields two open intervals under these rules. This is the
   deliberate direction of failure: an extra allergy on file is a false positive a clinician
   discounts; a missing one is the incident this whole design is shaped around. The correction
   path is explicit — `state: "ended"` from the extractor, `forget`/retraction from
   write-surfaces, or `facts resolve --action reject-new`. The extraction prompt change (§2.5)
   is what makes it reachable, and its quality is not testable offline: flag for the wave-6 review.
3. **Replaying machine decisions.** Choosing to replay only `keep_both` from the ~14.5k machine
   rows means a machine `close_old` can be re-litigated after a re-derivation. That is the
   intended trade (a stale supersession re-surfaces as a question rather than being silently
   re-applied), but it does mean a store that has been triaged and then heavily edited can see a
   small number of re-queued single-valued pairs. Measure in Phase 0.
4. **The drain estimate is unverified.** No number in §4.1 was measured against the live store,
   by constraint. If Phase 0 shows the remaining backlog above ~100 rows, the residue is
   `same_source_contradiction` and `comparable_values`, which are genuine questions and want a
   `facts conflicts --group-by subject` review flow rather than another classifier rule.
5. **`superseded_by` is doing two jobs.** In v0.3.1 it means both "this interval closed" and
   "this row was replaced". Keeping it populated for `close_reason = 'superseded'` preserves every
   existing reader, but a fact closed with reason `stopped` will have `valid_until` set and
   `superseded_by` NULL — so any reader that tests only `superseded_by IS NULL` for "current" will
   report a stopped medication as current. Every such site must move to
   `is_open_fact()` semantics. Known sites outside this lane: `sqlite.rs:3974-4030`,
   `:4150-4173`, `:4542-4760`, `commands/people.rs`, `retrieval.rs`. **This is the highest-risk
   cross-lane item in the spec** and needs an explicit sweep in wave 3 integration.
6. **The safety gate is enforced in `engram-graph`, not structurally in storage.**
   `engram-storage` cannot depend on `engram-graph` (the dependency runs the other way), so
   `close_fact_interval` cannot consult the registry. The gate is therefore a discipline
   (one funnel: `may_close`) plus an audit (`close_authority` column plus test 37 plus the Phase 3
   SQL assertion), not a type-level impossibility. If the lead wants it structural, the registry
   must move into `engram-core` — a larger change that this lane can absorb in wave 3 if decided
   before it starts.
7. **`--autonomy full` becomes weaker on safety relations**, by design: the LLM is a `Heuristic`
   authority and can no longer supersede an allergy. Agents using `--autonomy full` will see more
   queued pairs than before on medical content and fewer everywhere else. Worth a CHANGELOG line
   under breaking changes for agents.
8. **`predicate_arity("allergic_to")` flips from `Single` to `Multi`.** Anything outside this
   repo keying behaviour off that projection changes behaviour. Inside the repo the only consumer
   is the `arity` JSON field at `commands/facts.rs:436-439`, which is descriptive.

9. **Withdrawing `supersede-old` from `suggested_action` everywhere is broader than the brief
   asks.** The brief's move 2 kills `suggested_action: "supersede-old"` on SAFETY pairs
   (`fact_write.rs:1209-1213`); interfaces.md §3 likewise constrains only the safety
   member-attribute case. §1.4 of this spec goes further and removes the value from the
   suggestion vocabulary for every pair, safety or not. The argument for going further: an
   ordinary slot disagreement that reached the queue reached it precisely because the classifier
   could NOT order the two values, so suggesting a supersession there is the classifier laundering
   its own uncertainty as advice. The argument against: agents that auto-run
   `suggested_action` on routine slot queues will now do nothing where they used to converge, and
   the queue grows. **Open question for the lead**: narrow the change to safety pairs (brief-exact,
   smaller blast radius), or keep the blanket rule? The spec currently implements the blanket rule.
   Either way `supersede-old` remains an accepted `--action` value forever.

10. **The deprecated `slot_policy` shim (§2.5) does not round-trip.** It is defined by arity, so
    it necessarily changes two ways: safety SETS (`takes_medication`, `allergic_to`,
    `diagnosed_with`) map to `None` where they returned `AwaitConfirmation`, and `birthday`,
    `died_on`, `nationality` map to `LastWriterWins` where they returned `None` (they live in
    `SINGLE_VALUED_EXTRA`, not in `slot_policy`, today). The first is why `is_safety_slot` must be
    re-expressed against the registry rather than the shim (§2.1); the second changes
    `write_supersede_rule`'s audit tag for those three relations from `exact_match` to
    `slot_lww`, which shows up as different keys in `facts.resolution` and in the
    `resolved_by_rule` histogram. **Open question**: is the shim worth keeping at all? It has no
    in-repo caller left once `is_safety_slot`, `predicate_arity` and `write_supersede_rule` move
    to the registry, and a `#[deprecated]` function that answers differently than it used to is
    worse than a removed one. The spec keeps it for external callers; the lead may prefer to
    delete it and take the semver note.

11. **A cessation with nothing to close writes a fact nobody asserted.** §2.4 step 7 inserts a
    pre-closed interval when "stopped taking X" arrives and no open X exists, plus a
    `PlanNote::EndWithoutOpenInterval`. That is a defensible reading — the utterance does claim
    the person once took X — but it means a single sentence can create a historical medication
    record from nothing, and on a safety relation. The alternative is to record only the note and
    write no row, at the cost of losing the only trace of the statement. **Open question for the
    lead**; the spec currently inserts, because a fact the store can show you beats a fact it
    silently dropped, and the row is born closed so no reader treats it as current.

12. **Non-safety set relations auto-close on a member-attribute change (risk 1) has a second
    edge.** The v1 `member_identity` only strips from the END, so `has_role: "engineer"` and
    `"senior engineer"` are DIFFERENT members and coexist — the exact case risk 1 cites as the
    reason to auto-close never actually reaches the auto-close branch. What does reach it is
    dose-like and suffix-like variation on non-safety sets, where the heuristic is least tested.
    That weakens the argument for auto-closing at all and strengthens the "queue in v1, revisit
    after the drain is measured" option already listed in risk 1. Folded into that open question
    rather than decided here.

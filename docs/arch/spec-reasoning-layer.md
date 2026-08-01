# spec — reasoning-layer lane (engram v0.4, wave 5)

Status: implementation spec. Binding inputs, in precedence order:
`docs/arch/interfaces.md` **v2** (lead rulings R1–R15) > `docs/arch/redesign-brief.md` >
`docs/arch/coherence-report.md` (CD-4, CM-3, CM-11, CD-6) > `docs/arch/spec-fact-semantics.md`
(reviewed; this lane consumes it and adds no fact semantics of its own).

This lane is wave 5. Everything it reads is landed by waves 2–4. It writes no fact semantics, no
storage SQL of its own beyond one additive table, and no clap definitions.

---

## 1. Scope and non-goals

### 1.1 What this lane delivers

1. **`engram state <subject>`** — a compiled current-state object for one subject, built from
   `SqliteStore::open_intervals()` (spec-fact-semantics §6.2), grouped by relation via
   `engram_core::relations::relation_spec()`, carrying full provenance
   (fact → memory → chunk/document), the open questions still queued against that subject, and a
   full epistemic-read envelope (`returned`/`total`/`cursor`, `metadata.scope`,
   `data.verdict: "results" | "no_match"`, `data.absence`, calibrated `confidence`) per
   interfaces.md §4.
2. **A procedural/experience memory kind** — `engram state lessons` (list) and
   `engram state learn` (write): durable lessons *about the work* (what an agent tried, what the
   store told it, what the user corrected), stored as ordinary memories tagged
   `kind:procedural` plus a new `lessons` projection table so a lesson has structure (trigger,
   lesson, evidence events) instead of being free text in a haystack. Fed by two sources: the
   decision journal (`decision.recorded` events with `authority='caller'` — i.e. the user
   overruled the machine) and user corrections (`memory.revised`, `fact.interval_closed` with
   `close_reason='corrected'`).
3. **Reflection as a background job** — `engram state reflect` submits a `reflect` job through
   the daemon-jobs `Core` job API (interfaces.md §6), never running the LLM inline on the
   caller's thread. The job scans a journal window, proposes lessons, and writes them at
   `status='proposed'` until confirmed — `source_kind` is `agent_extracted` on the deterministic
   path and `compile_llm` when a model rewrote the sentence. `--inline` runs the same job body
   synchronously, for tests and for the no-daemon local-first path.
4. **`tests/state_*.rs`** — the lane's test files, every one isolated by `ENGRAM_STORE` +
   `TempDir` (constraint 1; CD-6).

### 1.2 Non-goals — explicitly other lanes' or later waves'

- **Interval semantics, contradiction, member identity, the registry.** `engram_core::relations`
  and `engram_core::interval` are storage-journal's files with fact-semantics' content
  (interfaces.md §0, R1). `classify_pair` / `safety_gate` / `SemanticKey` are `engram-graph`
  (fact-semantics). `state` calls them; it never re-derives a rule. If `state` needs a new
  predicate over intervals, it asks fact-semantics for it in that lane's file.
- **The clap subcommand.** `crates/engram-cli/src/cli.rs` is contract-codegen's, and under move 5
  the definition is *generated* from `crates/engram-contract`. This lane supplies the schema text
  and asks for it under `cross_lane_patches` (§6). CD-4 named this exact split.
- **The dispatch arm and `commands/mod.rs`.** `lib.rs` is daemon-jobs'; `commands/mod.rs` is the
  lead's (interfaces.md §7). Both are `cross_lane_patches`. CD-4 flagged `commands/mod.rs` as
  belonging to *no* lane; R15 gave it to the lead.
- **The job runner, queue, and daemon transport.** daemon-jobs owns `Core::submit_job`,
  `commands/jobs.rs`, `commands/serve.rs`. This lane registers ONE job kind (`reflect`) and
  supplies its body behind a trait object.
- **The envelope builder and `respond.rs`.** contract-codegen (R12). This lane calls it.
- **Retrieval, ranking, embeddings.** `state` does no vector search. Its inputs are projection
  rows, not similarity. (This is deliberate: a current-state answer that depends on a k-NN cutoff
  is not a current-state answer.)
- **Entity resolution quality.** `canonicalize_subject` (`fact_write.rs:515-556`) is reused
  verbatim; subject identity stays exactly as good or bad as it is today.
- **No new LLM call on any read path.** `state <subject>` and `state lessons` are deterministic
  and offline. Only `state reflect` calls a model, and only inside a job.
- **`has_age` derivation** (spec-fact-semantics §2.1 note: "deriving it is reasoning-layer work")
  is **deferred to v0.5**. It is a derived slot, not an open interval, and shipping it in the
  same wave as the state view would make `state`'s output partly computed and partly stored with
  no way for a caller to tell which. Recorded as open question Q3 (§7).

---

## 2. Data model and API changes

### 2.1 `engram state <subject>` — the surface

```
engram state <subject> [--kb <name>|*] [--diary <name>|*] [--relation <r>]
                       [--as-of <rfc3339|YYYY-MM-DD>] [--include-closed]
                       [--limit N] [--cursor <opaque>] [--json]
engram state lessons   [--kb <name>|*] [--trigger <slug>] [--status proposed|confirmed|retired]
                       [--limit N] [--json]
engram state learn <lesson> [--trigger <slug>] [--kb <name>] [--diary <name>]
                            [--evidence <event-handle>]... [--confidence F] [--offline] [--json]
engram state confirm <lesson-id> [--note <text>] [--json]
engram state retire  <lesson-id> [--reason <text>] [--json]
engram state reflect   [--kb <name>] [--since <rfc3339>] [--window N] [--inline]
                       [--offline] [--dry-run] [--json]
```

The reserved subcommand words are therefore **six**: `lessons`, `learn`, `confirm`, `retire`,
`reflect`, and `help` (clap's own). A subject with one of those names is addressed as
`engram state --subject confirm` (§7.1).

`<subject>` is a bare string, not a handle: the subject of a fact is a name, not an id
(`Fact::subject_norm`). `--kb` defaults to `*` (every KB) for the same reason `facts list` does —
defaulting to the KB literally named `default` made that command lie (`facts.rs:36-43`). `--diary`
defaults to `*`. `--limit` defaults to 200, `--as-of` defaults to now.

**Payload** (`data`, inside the standard envelope):

```jsonc
{
  "subject": "Ada Example",
  "subject_norm": "ada example",
  "resolved_from": "exact" | "alias" | "fuzzy" | "raw",
  "as_of": "2026-08-01T12:00:00Z",
  "verdict": "results",                    // or "no_match"
  "state": [
    {
      "relation": "takes_medication",      // canonical
      "arity_class": "set_valued",         // from relation_spec()
      "safety": true,
      "mutability": "mutable",
      "temporality": "state",
      "open_count": 2,
      "values": [
        {
          "handle": "fact:018f…", "id": "018f…",
          "object": "empagliflozin 12.5 mg daily", "object_norm": "…",
          "member": "empagliflozin",       // member_identity(), so a caller sees the grouping
          "valid_from": "2026-03-01T00:00:00Z", "valid_until": null, "open": true,
          "confidence": 0.91,
          "confidence_basis": { "extraction": 0.85, "source_kind": 0.05,
                                "confirmations": 0.03, "staleness": -0.02, "contested": 0.0 },
          "source_kind": "user_stated", "confirmations": 3,
          "provenance": {
            "memory": "mem:018f…", "chunk": "chunk:018f…", "document": "doc:018f…" | null,
            "observed_at": "2026-03-01T09:14:02Z", "snippet": "…240 chars…"
          },
          "history": [ /* closed intervals for the same member, only with --include-closed */ ]
        }
      ],
      "open_questions": [ /* conflict entries scoped to this relation */ ]
    }
  ],
  "open_questions": [
    { "handle": "conflict:018f…", "id": "018f…", "relation": "takes_medication",
      "new": { "handle": "fact:…", "object": "…" }, "old": { "handle": "fact:…", "object": "…" },
      "detector": "…", "suggested_action": "requires_context",
      "resolve": { "program": "engram", "args": ["facts","resolve","018f…","--action","keep-both"],
                   "allowed_actions": ["keep-both","reject-new"] },
      "resolve_command": "engram facts resolve 018f… --action keep-both" }
  ],
  "returned": 12,
  "total": 12,
  "cursor": null
}
```

**`RelationSpec` → payload mapping** (written here because `RelationSpec` is a nested enum and the
payload is flat; without this an implementer invents the strings):

| payload field | source | values |
|---|---|---|
| `arity_class` | `spec.arity` | `Arity::SetValued{..}` → `"set_valued"`, `Arity::SingleValued{..}` → `"single_valued"` |
| `safety` | the `safety` field inside either `Arity` variant | bool |
| `mutability` | `spec.mutability` | `"mutable"` \| `"immutable"` |
| `temporality` | `spec.temporality` | `"state"` \| `"instant"` |
| `value_domain` | `spec.value` | `"date"` \| `"text"` |

On `verdict: "no_match"` the `state` array is empty and `data.absence` is present:

```jsonc
"absence": { "top_score": 0.71, "threshold": 0.92, "searched_kbs": ["default","personal"],
             "searched_chunks": 0,
             "nearest_subjects": [ { "subject": "Adam Example", "score": 0.71 } ] }
```

`searched_chunks: 0` is honest and deliberate: `state` runs no retrieval, so there is no chunk
scan to report. `top_score`/`threshold` are the subject-resolution scores from
`canonicalize_subject`'s fuzzy branch (Jaro-Winkler, threshold 0.92, `fact_write.rs:544-556`),
not a similarity score, and `nearest_subjects` is the additive field that makes an absence
certificate actionable. `nearest_subjects` is an interface request to contract-codegen
(§6, request X-1).

**Wildcard scope is not a KB name.** `canonicalize_subject(ctx, diary, kb, raw)`
(`fact_write.rs:515-556`) takes `&str`, and its two helpers filter with equality:
`distinct_fact_subjects(diary, kb)` is `WHERE diary = ?1 AND kb = ?2`
(`sqlite.rs:4205-4218`) and `resolve_entity_alias(kb, …)` is likewise KB-scoped. Passing the
literal `"*"` — which is `state`'s DEFAULT for both flags — searches a KB and a diary named `*`,
returns nothing, and silently degrades every invocation to `resolved_from: "raw"` with an empty
`nearest_subjects`. Resolution rule, binding:

1. Expand the scope FIRST: `kb == "*"` → `list_kbs()`, `diary == "*"` → the distinct diaries in
   scope. Never pass `"*"` to a storage call.
2. Exact branch: `list_facts_by_subject_scoped(raw_norm, diary_opt, kb_opt, true)` — this one
   already takes `Option<&str>`, so the wildcard is `None` and it is ONE query, not a fan-out.
3. Alias branch: `resolve_entity_alias(kb, raw_norm)` per expanded KB, first hit wins in
   `list_kbs()` order (deterministic).
4. Fuzzy branch: union `distinct_fact_subjects(diary, kb)` over the expanded (kb, diary) pairs,
   dedup by `subject_norm`, then token-sorted Jaro-Winkler at threshold 0.92 — the same code
   `fact_write.rs:544-556` runs, over a wider candidate set.

Step 4 is the N×M fan-out that request **S-6** removes (`distinct_fact_subjects` gains
`Option<&str>` scoping). Until S-6 lands, the loop is correct and slow, and it is bounded by KB
count, not by fact count. Note also that `distinct_fact_subjects` filters `superseded_by IS NULL`
— acceptable for *nearest-neighbour naming*, which is a spelling hint and not a state answer, but
it means a subject whose every interval is closed can be missing from `nearest_subjects`. Recorded
as risk 12.

Exit code on `no_match` is **0** — an absence certificate is a successful answer, not an error
(interfaces.md §4; move 5 kills exit-0-lying, not exit-0-answering). Exit 3 (`NotFound`) is
reserved for a malformed handle passed to `--evidence`.

### 2.2 `commands/state.rs` — public Rust surface

```rust
//! crates/engram-cli/src/commands/state.rs  (NEW, this lane's file)

use crate::context::AppContext;
use crate::error::CliError;

pub struct StateArgs {
    pub subject: String,
    pub kb: String,                 // "*" = every KB
    pub diary: String,              // "*" = every diary
    pub relation: Option<String>,   // raw or canonical; canonicalized internally
    pub as_of: Option<DateTime<Utc>>,
    pub include_closed: bool,
    pub limit: usize,
    pub cursor: Option<String>,
}

/// `engram state <subject>` — compile the current-state object and print it.
pub fn run(ctx: &AppContext, args: StateArgs) -> Result<(), CliError>;

/// Pure-ish core, split out so tests and `serve.rs` (daemon parity, wave 6)
/// can build the payload without printing.
pub fn compile_state(ctx: &AppContext, args: &StateArgs) -> Result<StateView, CliError>;

pub struct StateView {
    pub subject: String,
    pub subject_norm: String,
    pub resolved_from: SubjectResolution,
    pub as_of: DateTime<Utc>,
    pub groups: Vec<RelationGroup>,
    pub open_questions: Vec<serde_json::Value>,
    pub returned: usize,
    pub total: usize,
    pub cursor: Option<String>,
    pub scanned_kbs: Vec<String>,
    pub absence: Option<Absence>,
}
impl StateView { pub fn to_json(&self) -> serde_json::Value; }

pub enum SubjectResolution { Exact, Alias, Fuzzy { score: f64 }, Raw }

pub struct RelationGroup {
    pub relation: String,
    pub spec: engram_core::relations::RelationSpec,
    pub values: Vec<StateValue>,
    pub open_questions: Vec<serde_json::Value>,
}

pub struct StateValue {
    pub fact: engram_core::types::Fact,
    pub member: String,
    pub confidence: f32,
    pub basis: ConfidenceBasis,
    pub provenance: Provenance,
    pub history: Vec<engram_core::types::Fact>,
}

pub struct Provenance {
    pub memory_id: Uuid,
    pub chunk_id: Option<Uuid>,
    pub document_id: Option<Uuid>,
    pub observed_at: DateTime<Utc>,
    pub snippet: String,
}
```

### 2.3 Calibrated confidence — the formula, pinned

interfaces.md §4 requires "calibrated confidence" on every read and does not define it. This lane
defines ONE deterministic function and publishes its terms, because a confidence number with no
stated basis is the same defect as a count that means a page.

```rust
pub struct ConfidenceBasis {
    pub extraction: f32,    // Fact::confidence as stored
    pub source_kind: f32,   // +0.05 user_stated/user_confirmed, 0.0 agent_extracted,
                            // -0.05 compile_llm, -0.02 import
    pub confirmations: f32, // +0.02 per confirmation, capped +0.06
    pub staleness: f32,     // -0.02 per full year since valid_from, capped -0.10;
                            // 0.0 when relation_spec().mutability == Immutable
    /// Some(-0.15) if this fact is a side of an OPEN conflict row, Some(0.0) if
    /// it is not, and None when the conflict cap of risk 8 was hit and we did
    /// not look. None serializes as JSON `null` and contributes 0.0 to the sum.
    /// A `false`/`0.0` that means "we did not look" is the count-meaning-page
    /// defect in miniature, which is why this term alone is optional.
    pub contested: Option<f32>,
}
/// Sum of the basis terms, clamped to [0.01, 0.99]. Never 1.0: an extracted
/// belief is never certain, and a caller who sees 1.0 stops checking.
pub fn state_confidence(basis: &ConfidenceBasis) -> f32;
```

`Fact::valid_from` is `Option<DateTime<Utc>>` (`types.rs:172-174`), so staleness measures from
`valid_from.unwrap_or(created_at)` — a fact with no event time is as stale as its observation, not
infinitely fresh. The constants live as `const` items at the top of `state.rs` in the repo's
WHY-comment register. Tests 6–10 (§5) pin every term and the clamp.

### 2.4 Lessons — the procedural/experience memory kind

A lesson is a durable claim about **the work**, not about the world: "when a caller resolves a
`takes_medication` conflict with keep-both, do not re-queue that pair after an edit". It has a
trigger (when it applies), a lesson (what to do), evidence (journal event ids), and a status.

```rust
// engram_storage (REQUEST to storage-journal — §6, request S-1)
pub struct LessonRow {
    pub id: Uuid,
    pub kb: String,
    pub diary: String,
    pub trigger: String,          // slug: [a-z0-9_]{1,64}
    pub lesson: String,
    pub lesson_norm: String,      // lowercased, whitespace-collapsed; dedup key
    pub status: String,           // "proposed" | "confirmed" | "retired"
    pub source_kind: String,      // SourceKind::as_str()
    pub confidence: f32,
    pub occurrences: u32,
    pub evidence: serde_json::Value,   // ["<event uuid>", …]
    pub memory_id: Option<Uuid>,       // the recallable memory twin, if written
    pub rule: Option<String>,          // deterministic proposer rule id, if machine-made
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
```

**DDL — schema v11** (requested from storage-journal; v10 is the journal migration):

```sql
CREATE TABLE IF NOT EXISTS lessons (
    id            TEXT PRIMARY KEY,
    kb            TEXT NOT NULL DEFAULT 'default',
    diary         TEXT NOT NULL DEFAULT 'lessons',
    trigger       TEXT NOT NULL,
    lesson        TEXT NOT NULL,
    lesson_norm   TEXT NOT NULL,
    status        TEXT NOT NULL DEFAULT 'proposed',
    source_kind   TEXT NOT NULL DEFAULT 'agent_extracted',
    confidence    REAL NOT NULL DEFAULT 0.5,
    occurrences   INTEGER NOT NULL DEFAULT 1,
    evidence_json TEXT NOT NULL DEFAULT '[]',
    memory_id     TEXT,
    rule          TEXT,
    created_at    TEXT NOT NULL,
    updated_at    TEXT NOT NULL,
    FOREIGN KEY(kb) REFERENCES knowledge_bases(name) ON DELETE CASCADE
);
-- Dedup: the same lesson for the same trigger in the same KB is ONE row whose
-- occurrences counter rises. Without this a nightly reflect job re-proposes the
-- same sentence forever and `state lessons` becomes the 951-row backlog again.
CREATE UNIQUE INDEX IF NOT EXISTS idx_lessons_dedup ON lessons(kb, trigger, lesson_norm);
CREATE INDEX IF NOT EXISTS idx_lessons_status ON lessons(kb, status, updated_at);
```

`memory_id` is deliberately **not** a foreign key: a lesson must survive the retraction of its
memory twin. (Under move 1 nothing is hard-deleted, but `memories.deleted = 1` rows exist from
before v10, and a FK would still cascade under an `admin rebuild` that drops projections.)

Three new `EventKind` variants (request S-2): `LessonRecorded` (`lesson.recorded`),
`LessonConfirmed` (`lesson.confirmed`), `LessonRetracted` (`lesson.retracted`). Payloads:

- `lesson.recorded` — `{ lesson_id, kb, diary, trigger, lesson, source_kind, rule?, evidence: [event_id], memory_id?, occurrences }`
- `lesson.confirmed` — `{ lesson_id, actor, note? }`
- `lesson.retracted` — `{ lesson_id, actor, reason? }`

`subject_id` on the event row is the lesson id, so `events_for_subject(lesson_id)` returns a
lesson's whole life for free and `admin rebuild` can replay the `lessons` projection.

**The memory twin.** `state learn`, and a lesson at the moment it is confirmed, also write the
lesson as an ordinary memory so `recall` finds it — the lesson layer must not be a silo an agent
has to already know about.

It CANNOT be done by calling `commands::remember::run`. That function is the command entry point
and it always prints: `remember.rs:171` ends in `print_success` (`:269`, `:568`). Calling it from
`state learn` would emit a `remember` envelope on stdout followed by the `lesson` envelope, and a
caller that parses one JSON document off stdout gets a lie — the exact class move 5 exists to
kill. Its argument struct also does not have the shape a caller would guess: `RememberArgs`
(`remember.rs:40-59`) has `diary: String`, `kb: String`, `autonomy: String` — all plain, none
optional — and no `Option` wrapper anywhere.

So this lane files request **W-1** to write-surfaces: split the printing shell from the write.

```rust
// crates/engram-cli/src/commands/remember.rs  (REQUEST W-1, write-surfaces)
pub struct RememberOutcome { pub memory_id: Uuid, pub extraction: &'static str,
                             pub duplicate_of: Option<Uuid> }
/// Everything `run` does except printing. `run` becomes
/// `write_memory(...)` + `print_success(...)`, so there is exactly one write path.
pub async fn write_memory(ctx: &AppContext, args: &RememberArgs)
    -> Result<RememberOutcome, CliError>;
```

Called with the current field types, not invented ones:

```rust
RememberArgs {
    content: format!("Lesson ({trigger}): {lesson}"),
    importance: 7,
    tags: vec!["kind:procedural".into(), "lesson".into(), format!("trigger:{trigger}")],
    diary,                            // String, already validated for write
    kb,                               // String, already ensured to exist
    no_extract_facts: true,           // a lesson is not a world fact
    force_extract_facts: false,
    allow_duplicate: false,
    autonomy: "strict".into(),        // String, not Option; unused when extraction is off
    facts_json: None,
    event_time: None,
    offline,                          // from `--offline` on `state learn` (§2.1)
}
```

If W-1 is refused, the twin is written directly through `Core::write` with
`insert_memory_with_kb` + `memory.written`, and `state learn` loses nothing except code reuse.
What it may NOT do is call the printing function: two envelopes on one stdout is not shippable.

**Ordering and atomicity.** Journal invariant 1 (interfaces.md §2) — projection mutation and its
event append are ONE transaction — applies to lessons too. The order inside a single
`Core::write` closure is:

1. `ensure_kb(kb)` — the `lessons` DDL has `FOREIGN KEY(kb) REFERENCES knowledge_bases(name)`, so
   a first `state learn --kb newkb` fails the FK if the row is inserted before the KB exists.
2. `write_memory` (or the direct insert) → `memory_id`, if a twin is due at this status.
3. `upsert_lesson(row with memory_id)`.
4. `append_event(lesson.recorded)`.

Steps 1–4 commit together or not at all. There is no state in which a `lessons` row exists
without its `lesson.recorded` event (which would make `admin rebuild` lose it) or in which a twin
memory exists with no lesson pointing at it.

`no_extract_facts: true` is load-bearing: without it, "when a caller resolves a takes_medication
conflict…" extracts a `takes_medication` fact and the reflection layer poisons the fact layer.
Test 21 pins it.

`kind:procedural` is a tag, not a new `MemorySource` variant and not a new column: tags are
already indexed, exported, and filterable, and adding an enum variant to
`engram_core::types::MemorySource` would break every existing `source_json` reader on downgrade.

### 2.5 The reflection job

```rust
// crates/engram-cli/src/commands/state.rs
pub struct ReflectParams {
    pub kb: String,
    pub since_seq: Option<i64>,   // None = resume from the last reflect job's high-water mark
    pub window: usize,            // max events to scan, default 2000
    pub offline: bool,            // deterministic proposers only, no LLM
    pub dry_run: bool,
}
pub struct ReflectStats { pub events_scanned: usize, pub candidates: usize,
                          pub lessons_written: usize, pub lessons_merged: usize,
                          pub high_water_seq: i64 }

/// The job body, expressed as daemon-jobs' `JobRunner` (spec-daemon-jobs §4.4), which is
/// the shape their registry actually takes — `registry.register(Arc::new(ReflectRunner))`
/// is already the one-liner their §4.4 promises for wave 5.
pub struct ReflectRunner;
impl engram_cli::engine::JobRunner for ReflectRunner {
    fn kind(&self) -> &'static str { "reflect" }
    /// Rejects a bad window/kb/since at submit time, not inside a worker.
    fn validate(&self, params: Value) -> Result<Value, CliError>;
    fn run<'a>(&'a self, jc: &'a JobCtx, params: &'a Value, cursor: &'a Value) -> JobFuture<'a>;
}
/// The body both paths share. `run` deserializes `params`/`cursor` and calls this;
/// `--inline` reaches it through the same job row (see below), so the no-daemon path
/// and the tests run the SAME code.
pub async fn reflect_job(ctx: &AppContext, p: &ReflectParams) -> Result<ReflectStats, CliError>;
```

**`--inline` is not a bypass.** It is daemon-jobs' documented "no daemon, CLI foreground" path
(spec-daemon-jobs §4.6): `submit_job` inserts a `queued` row, the same process claims it and runs
it, checkpointing through `JobCtx::checkpoint`. That matters here for one concrete reason — the
high-water mark lives in the job row (`cursor` / `metadata_json`), so `--inline` needs a job row
to be resumable at all. An `--inline` that skipped the row would have nowhere to persist
`high_water_seq`, and tests 27–28 (idempotence, resumption) would have nothing to assert against.
`--dry-run` submits nothing and writes nothing.

Pipeline, in order:

1. **Read the window.** `events_since(kinds, from_seq, limit)` (request S-3) over SIX kinds, not
   four. The trigger kinds — what the work taught us — are `decision.recorded` where
   `authority == "caller"`, `memory.revised`, `memory.retracted`, and `fact.interval_closed`
   where `reason == "corrected"`. But two proposers need an ANTECEDENT the trigger event does not
   carry, so `memory.written` and `derivation.ran` are read in the same pass. These are still
   exactly "the decision journal and user corrections" from the brief's stage 5; the extra two
   kinds are context, never candidates.
   - The payload field on `fact.interval_closed` is named **`reason`**, not `close_reason`
     (interfaces.md §2 event payload list). `close_reason` is the projection COLUMN name. Using
     the column name against the payload matches nothing and the proposer fires zero times.
2. **The antecedent lookback.** A `memory.revised` at seq 900 whose `memory.written` sits at seq
   40 — below the window floor — is the common case, not the edge case: a window is a few thousand
   events and a memory can be corrected months after it was written. So the proposers are not
   pure over the window alone. Each trigger event resolves its antecedent by subject:
   `events_for_subject(memory_id)` for `memory.revised`/`memory.retracted` (giving `memory.written`
   and `derivation.ran` for that memory at any seq), and `lookup_decision(kb, decision_key)` for
   the machine side of a caller decision. The proposer functions therefore take
   `fn(&[EventRow], &dyn Antecedents) -> Vec<LessonCandidate>`, where `Antecedents` is a small
   trait with two methods — a `HashMap`-backed fake in unit tests, the store in production. A
   proposer that silently drops every lesson whose cause predates the window is a proposer that
   works only on a store younger than one window.
3. **Deterministic proposers.** Offline, testable, each with a stable `rule` id:
   - `caller_overruled_machine` — a caller `decision.recorded` whose `semantic_key` already had a
     `machine` decision with a different verdict. Trigger: `conflict_<relation>`, where
     `<relation>` comes from `SemanticKey`'s `relation` field via
     `engram_graph::SemanticKey::parse(&payload.semantic_key)` — NOT from string-splitting the
     encoded key, whose format is `engram-graph`'s to change (R9).
   - `correction_within_window` — a `memory.revised` less than 15 minutes after that memory's
     `memory.written` (found via the lookback of step 2). Trigger: `fresh_write_correction`.
   - `repeat_close` — the same (subject_norm, relation) closed ≥ 3 times in the window.
     Trigger: `churny_slot_<relation>`. **`fact.interval_closed`'s payload carries only
     `fact_id`** (interfaces.md §2) — no subject, no relation — so this proposer resolves each
     closed fact through `get_fact(fact_id)` to group them. It is the one proposer that is not
     pure over event rows; the alternative (asking storage-journal to widen the payload) is a
     change to a binding event schema for one heuristic, which is the worse trade.
   - `retraction_after_extraction` — a `memory.retracted` whose `derivation.ran` (same lookback)
     produced ≥ 1 fact, read from that event's `fact_count`. Trigger: `bad_extraction_source`.
   Each emits `LessonCandidate { rule, trigger, evidence: Vec<Uuid>, summary: String }` where
   `summary` is a templated sentence — so the offline path produces real lessons with no model.
4. **LLM pass** (skipped under `p.offline`, `--dry-run`, or no configured key). ONE call per batch
   of ≤ 20 candidates, never one per candidate. It only rewrites the templated summary into an
   imperative sentence and may DROP a candidate. It may not invent a candidate, change a trigger,
   or touch evidence — those are structural and come from the journal; a response naming an
   unknown candidate index is discarded, not trusted. Usage is recorded through the same
   accounting `compile` uses so `engram usage` and budgets stay honest.
5. **Write.** Each survivor → `upsert_lesson` (request S-1): insert at `status='proposed'`, or on
   unique-index hit bump `occurrences`, union `evidence`, refresh `updated_at`, and leave
   `status`/`lesson` alone (a confirmed lesson is never silently re-worded by a later machine
   pass). Append `lesson.recorded`. No memory twin at `proposed` status — the twin appears on
   confirmation, so `recall` never surfaces an unreviewed machine sentence as if the user said it.
6. **High-water mark.** `high_water_seq` (max `events.seq` scanned) is checkpointed through
   `JobCtx::checkpoint` into the job row's cursor, which is why `--inline` still creates one. The
   next run resumes there, so reflection is resumable and re-running over an already-scanned
   window is a no-op. Resolution order for the floor, no ambiguity: `--since <rfc3339>` (converted
   to a seq, below) > the last completed `reflect` job's `high_water_seq` for this KB > 0.

**`--since` is a timestamp; the journal cursor is a seq.** `events.seq` is a monotonic
AUTOINCREMENT and `events.ts` is transaction time (interfaces.md §2), so the conversion is a
lookup, not arithmetic. Request **S-3** therefore carries a second method:
`pub fn seq_at_or_after(&self, ts: DateTime<Utc>) -> Result<i64, StorageError>` —
`SELECT COALESCE(MIN(seq), <max seq + 1>) FROM events WHERE ts >= ?1`. Filtering by `ts` in Rust
after fetching the window is NOT equivalent: the window is capped at `--window N` rows from the
floor, so a `ts` filter applied after the cap scans the wrong N rows.

`--dry-run` runs steps 1–3 and prints candidates without writing a row, an event, or a job.

---

---

## 3. File-by-file change plan

### 3.1 `crates/engram-cli/src/commands/state.rs` — NEW, ~750 lines, this lane's only source file

Module header in the repo's WHY register: why a compiled state object exists at all (an agent
that asks `facts show ada` today gets a flat list where a stopped medication and a current one
look identical, because both have `superseded_by IS NULL`; risk item 5 of spec-fact-semantics),
and why it does no retrieval.

| item | kind | what it does |
|---|---|---|
| `const CONF_*` | consts | the pinned confidence terms of §2.3 |
| `pub fn run(ctx, StateArgs)` | pub | `compile_state` → `to_json` → `print_success` (human renderer groups by relation, marks safety slots, prints open questions last) |
| `pub fn compile_state(ctx, &StateArgs) -> Result<StateView, CliError>` | pub | the pipeline of §3.2 |
| `fn resolve_subject(ctx, diary, kb, raw) -> (String, String, SubjectResolution)` | priv | the four branches of `crate::fact_write::canonicalize_subject` (`fact_write.rs:515-556`) in the same order and with the same 0.92 threshold, but over the EXPANDED scope of §2.1 and REPORTING which branch matched. It cannot literally wrap that function for two reasons: it takes `kb: &str`/`diary: &str` and would search a KB named `*`, and it returns only the `(display, norm)` pair — `resolved_from` is what tells a caller "I answered about a different person than you asked about". `fact_write.rs` is fact-semantics' file, so the branch order is *mirrored*, not edited; if the write path's resolution rules change, this is the second call site to update, and §7 risk 12 flags it |
| `fn nearest_subjects(ctx, diary, kb, raw, n) -> Vec<(String, f64)>` | priv | Jaro-Winkler over `SqliteStore::distinct_fact_subjects` (`sqlite.rs:4205`), token-sorted exactly as `fact_write.rs:557+` does, over the EXPANDED scope of §2.1 (never the literal `"*"`); feeds `absence` |
| `fn group_by_relation(facts) -> Vec<RelationGroup>` | priv | canonicalizes via `engram_core::relations::canonical_relation`, looks up `relation_spec`, computes `member_identity` per value |
| `fn provenance_for(ctx, &Fact) -> Result<Provenance, CliError>` | priv | `chunks_for_memory(memory_id)` (request **S-7**) for the chunk + snippet, `document_for_memory` (`sqlite.rs:2731`), `memory_times` (`sqlite.rs:2784`) for `observed_at`; snippet reuses the 240-char rule of `people.rs:442-450`. **Not** `list_chunks_by_memory` — despite the name, its signature is `(kb: Option<&str>)` (`sqlite.rs:1805-1808`) and it lists every live chunk in a KB, so using it per fact is a full-KB scan per value |
| `fn confidence_for(&Fact, &RelationSpec, contested: bool, as_of) -> (f32, ConfidenceBasis)` | priv | §2.3 |
| `fn open_questions_for(ctx, subject_norm, diary, kb) -> Vec<Value>` | priv | `list_open_fact_conflicts_with_facts(diary, kb, Some(subject_norm), cap)` (`sqlite.rs:4320`), rendered through contract-codegen's `respond::resolve_payload` (R12) with `allowed_actions` from `engram_graph::semantics::allowed_actions` |
| `fn encode_cursor/decode_cursor` | priv | opaque base64 of `(relation, fact_id)`; a cursor from a different subject or scope is `BadInput`, never silently reinterpreted |
| `pub fn lessons(ctx, LessonsArgs)` | pub | `list_lessons` → envelope with `returned`/`total` |
| `pub async fn learn(ctx, LearnArgs)` | pub | validate trigger slug + `--evidence` handles (`Handle::parse`, kind `event`), `commands::mod::validate_diary_for_write` (`mod.rs:44`), then the single-transaction sequence of §2.4: `ensure_kb` → memory twin via `remember::write_memory` (request W-1, NOT the printing `run`) → `upsert_lesson` → `lesson.recorded` |
| `pub async fn confirm/retire(ctx, id, note)` | pub | status transitions + `lesson.confirmed`/`lesson.retracted`; `confirm` writes the memory twin if absent |
| `pub async fn reflect(ctx, ReflectParams, inline: bool)` | pub | `Core::submit_job(&NewJob{kind:"reflect", …})` in BOTH cases (daemon-jobs' §4.6 foreground path), then print the handle; `inline` additionally claims and runs the row in-process so the high-water mark still has a durable home |
| `pub struct ReflectRunner` | pub | the `JobRunner` impl of §2.5 — `kind()`/`validate()`/`run()`; `run` deserializes params + cursor and calls `reflect_job` |
| `pub async fn reflect_job(ctx, &ReflectParams)` | pub | §2.5 pipeline |
| `mod proposers` | priv mod | the four deterministic proposers, each `fn(&[EventRow], &dyn Antecedents) -> Vec<LessonCandidate>`. `Antecedents` (`events_for_subject`, `fact_of`) is the one seam through which a proposer may reach outside its window (§2.5 step 2); unit tests pass a `HashMap`-backed fake and still need no store |
| `#[cfg(test)] mod tests` | tests | pure-function units: confidence terms, cursor round-trip, trigger-slug validation, proposer rules over hand-built `EventRow`s |

### 3.2 `compile_state` — the pipeline, in order

1. `resolve_subject` → `(display, subject_norm, resolved_from)`.
2. Scope: `kb == "*"` → `None` filter, else `Some(kb)`; same for diary. Record the KB list
   actually scanned (`SqliteStore::list_kbs`) into `scanned_kbs` for `metadata.scope.kbs`.
3. `store.open_intervals(kb, diary, subject_norm, relation.as_deref())` per (kb, diary) in scope
   — spec-fact-semantics §6.2. **This is the only source of "current".** `state` never filters on
   `superseded_by IS NULL` itself; that predicate is exactly the bug risk item 5 names.
4. `--as-of T` filters the interval set. Two things the previous draft left for the implementer to
   invent, both now pinned:

   **(a) There is no `Interval::contains`.** `engram_core::interval` ships `is_open`, `overlaps`,
   `precedes`, `is_point` (interfaces.md §3.2) and nothing else. This lane does not add a method to
   another lane's file, so containment is a private helper in `state.rs`:

   ```rust
   /// Half-open [from, until) — EXCEPT for point intervals, where until == from
   /// and a half-open test can never be true, so an `Instant` fact would vanish
   /// from every --as-of query including one asked at its own instant.
   fn contains(iv: &Interval, t: DateTime<Utc>) -> bool {
       if iv.is_point() { return iv.from == t; }
       iv.from <= t && iv.until.map_or(true, |u| u > t)
   }
   ```

   Every `Instant` relation in registry v1 is `Immutable` (interfaces.md §3.2), so a birthday is
   a point interval and `state ada --as-of 2026-01-01` must still report it. Test 13b pins it.

   **(b) `facts_for_relation` cannot serve this query.** Its signature is
   `(kb: &str, diary: &str, subject_norm: &str, relation: &str, include_closed: bool)`
   (spec-fact-semantics §6.2) — `relation` is required, and `state <subject>` without `--relation`
   is a query over ALL relations. Same for `--include-closed`, which needs the closed history of
   every relation, not one. Request **S-8** covers both: `open_intervals` gains an
   `include_closed: bool` parameter (it already takes `relation: Option<&str>`), so one method
   answers open-now, as-of-then, and with-history. If S-8 is refused, the fallback is
   `open_intervals` for the relation LIST and one `facts_for_relation` per relation found — a
   fan-out over relations, correct and slower, and it is the only fallback that exists.
5. `group_by_relation`, sorted: safety relations first, then alphabetically; values within a
   group sorted by `member`, then `valid_from` descending, then fact id (fully deterministic —
   two runs on the same store must byte-match, which test 4 asserts).
6. `total` = the ungrouped count of matching intervals BEFORE `--limit`; `returned` = after.
   Never the page size. (Move 5: "agents reported 3 conflicts when 953 were open".)
7. `open_questions_for`, attached both flat and per-relation-group. **The storage filter is a
   substring match, not an equality match**: `list_open_fact_conflicts_with_facts` builds
   `format!("%{}%", subject.trim().to_lowercase())` and compares with LIKE (`sqlite.rs:4320-4335`).
   So `state ada` would inherit Adam Example's conflicts. `open_questions_for` re-filters the
   returned rows on `fact.subject_norm == subject_norm` before rendering. A state view that
   reports another person's open question is worse than one that reports none.
8. Envelope wiring, not left to the renderer: `metadata.scope { kbs, diary, chunks_considered }`
   (interfaces.md §4) is populated from `StateView` at print time — `kbs` from `scanned_kbs`,
   `diary` from the requested (possibly `*`) diary, `chunks_considered: 0`, honest for the same
   reason `absence.searched_chunks` is 0. It is passed through the existing `Metadata` struct that
   `print_success` already takes (`output.rs`), so no new output plumbing is requested.
9. Empty result → `verdict: "no_match"` + `absence` from `nearest_subjects`; otherwise
   `verdict: "results"`. A subject that resolved through the **fuzzy** branch is
   `verdict: "results"` about a DIFFERENT NAME than the caller typed, so the payload carries
   `resolved_from: "fuzzy"` with its score AND the same `nearest_subjects` array the absence
   certificate carries — an agent that only reads `state[]` must still be able to see, in the
   fields it does read, that it was answered about Adam when it asked about Ada. Test 12b pins it.

### 3.3 Tests — `crates/engram-cli/tests/state_*.rs` (this lane's files)

- `tests/state_view.rs` — the `engram state <subject>` contract (tests 1–15).
- `tests/state_lessons.rs` — `learn` / `lessons` / `confirm` / `retire` (tests 16–24).
- `tests/state_reflect.rs` — the reflection job (tests 25–33).

Every test constructs its own `TempDir` and sets `ENGRAM_STORE` to it (§5.1). No test in these
files calls `point_at_home`, and none reads `HOME`, `XDG_*`, or `ProjectDirs`.

### 3.4 Files this lane does NOT edit (see §6)

`cli.rs`, `lib.rs`, `commands/mod.rs`, `agent_info.rs`, `assets/skill.md`,
`crates/engram-core/src/{events,handle}.rs`, `crates/engram-storage/src/sqlite.rs`,
`crates/engram-cli/src/commands/{jobs,serve,remember}.rs`, `crates/engram-cli/src/fact_write.rs`.

---

## 4. Migration and compatibility

### 4.1 There is no data to migrate

`engram state` is a pure read over projections that waves 2–3 already built. It adds no column to
`facts`, `memories`, or `fact_conflict_queue`, and it rewrites nothing. A v10 store answers
`engram state` correctly the moment the binary lands: every open interval it reads was created by
the v9→v10 backfill (interfaces.md §2.1 step 2, "rows with `superseded_by NULL` keep `valid_until`
NULL: they are the open intervals, and they are what `engram state` will read").

### 4.2 Schema v11 — additive, forward-only, empty on arrival

The only DDL is §2.4's `lessons` table. Migration v10→v11 is `CREATE TABLE IF NOT EXISTS` +
two indexes and nothing else: no backfill, no data rewrite, no `VACUUM INTO` backup (the v10
backup rule of interfaces.md §2.1 exists because v10 rewrites existing rows; v11 does not touch
one). It must be idempotent and must run for `open_in_memory()` like every other `CREATE TABLE`
step. `CURRENT_VERSION` at `sqlite.rs:792` goes 9 → 10 (storage-journal) → 11 (this request).

**Downgrade.** Say this precisely, because the first draft said two contradictory things in two
sentences. A v10 binary opening a v11 store **refuses**: `migrate()` errors when
`version > CURRENT_VERSION` (`sqlite.rs:878-882`, "upgrade engram instead of downgrading the
database"). That is the pre-existing behaviour for every version bump and this lane does not
change it. The narrower claim, which is the one that matters and the one that is true: **no v11
write corrupts anything a v10 binary reads.** `lessons` is a new table, no existing table gained a
column, and the memory twins are ordinary `memories` rows a v10 binary already understands.
Brief constraint 3 ("the existing CLI surface keeps working") is about a current binary against
an upgraded store, not about a stale binary against a future one.

**Hard dependency on wave 2/3, not on `main`.** The `history` payload emits `close_reason` and
`valid_until` per value. `valid_until` exists on `Fact` today (`types.rs:174-177`);
`close_reason`/`closed_by`/`close_authority` do NOT — they are the additive columns interfaces.md
§3.3 assigns to storage-journal. `state` therefore does not compile until that lands. Stated here
as a gate rather than assumed: the anchors in §6 are `main@0.3.1` line numbers, but the TYPES this
lane consumes are wave-2/3 deliverables.

### 4.3 Backward compatibility of the surface

- **`state` is a new command.** No existing command's JSON changes. Nothing this lane ships can
  break a caller that never types `engram state`.
- **Bare UUIDs.** `--evidence` accepts `event:<uuid>` and bare `<uuid>` (interfaces.md §1). A
  handle of the wrong kind is `WrongIdKind` naming the actual kind, exit 3.
- **There is no `lesson:` handle kind unless the lead grants one.** `HandleKind` is the closed set
  `mem | chunk | fact | conflict | entity | doc | job | event` (`handle.rs:18-27`, interfaces.md
  §1), so `Handle::parse("lesson:…")` is `UnknownKind` today. Printing a `lesson:` handle would
  invent a divergent shape, which the brief forbids. Request **X-2** (§6.2) asks for the variant;
  **until it is granted, a lesson id is serialized as a bare uuid in `id` with no `handle` field**,
  and `state confirm`/`retire` accept a bare uuid. Test 16 asserts whichever of the two is in
  force, not both.
- **Exit codes.** 0 for results AND for `no_match`; 3 for a bad subject-free invocation, a bad
  cursor, a bad trigger slug, a wrong-kind `--evidence` handle, or an unknown lesson id; 2 when
  `state reflect` is asked for an LLM pass with no key and no `--offline`; 4 on rate limit from
  the reflect LLM call. This matches the taxonomy in interfaces.md §4 with no reinterpretation.
  **"Asked for an LLM pass" is defined, not left to feel**: it means `!offline && !dry_run`. The
  check fires BEFORE step 1, at `validate()`, so an unkeyed caller learns in milliseconds and
  never gets a zero-lesson success that looks like "nothing to learn". `--offline` with no key is
  exit 0 and produces the templated proposer lessons — the deterministic path is the product, not
  a fallback, and a caller who wants it says so.
- **Memory twins are ordinary memories.** `export`/`import` (v3 journal slice, storage-journal)
  carry them with no special case. A `lessons` row lost in an old export is rebuildable from
  `lesson.recorded` events, which is the point of putting them in the journal.
- **`bench personal` is untouched.** `state` is not on any bench path; the gate is that
  `engram bench personal --json` output is byte-identical before and after this lane.

### 4.4 Degradation if request S-1 (the `lessons` table) is refused

If the lead declines a v11 migration in this wave, `state lessons`/`learn`/`reflect` degrade to
memory-only: lessons are written solely as `kind:procedural` memories, `state lessons` lists them
via a tag scan, and dedup/`occurrences`/`status` are dropped. `engram state <subject>` — the
headline deliverable — is unaffected, because it needs no new table. **This lane does not stub
the storage methods in `engram-cli`**; it takes the documented degradation or blocks, for the
same reason spec-fact-semantics §6.8 refuses to stub `binding_decision`.

Which tests survive that degradation, named now so nobody discovers it at integration: **16, 17,
19, 20, 21, 22 pass unchanged** (they assert the write, the listing, validation, and the twin).
**18 (dedup/`occurrences`), 23 and 24 (status transitions, retire-does-not-delete), 27 (dedup on
re-run), and 29's "`lessons` table empty" assertion do not** — they test the table. Under the
degradation those five ship `#[ignore]`d behind a named gate owned by the lead, with the ignore
reason string naming request S-1. An `#[ignore]` with no named gate is a guarantee that ships
untested (CD-3).

---

## 5. Test plan

### 5.1 Isolation — the rule every test in this lane obeys

```rust
/// Every test gets its own store. `ENGRAM_STORE` roots db, vectors, config and
/// cache under one TempDir (paths.rs, LANDED), so no test can reach
/// ~/Library/Application Support/bio.199-biotechnologies.engram/ — the store
/// that a HOME-only isolation bug once migrated and bricked.
fn engram(store: &TempDir) -> assert_cmd::Command {
    let mut cmd = assert_cmd::Command::cargo_bin("engram").expect("binary built");
    cmd.env("ENGRAM_STORE", store.path());
    cmd.env_remove("ENGRAM_HOME");        // interfaces.md R14: this name is an error
    cmd.env_remove("GEMINI_API_KEY");
    cmd.env_remove("COHERE_API_KEY");
    cmd.env_remove("OPENROUTER_API_KEY");
    cmd.env_remove("ENGRAM_FACT_LLM_STUB");
    cmd.env_remove("ENGRAM_COMPILER_LLM_STUB");
    cmd.env_remove("ENGRAM_REFLECT_LLM_STUB");
    cmd.env("ENGRAM_BENCH_FORCE_STUB", "1");
    cmd
}
```

`point_at_home` does not appear in `tests/state_*.rs`. A grep gate in test 33 asserts it:
`assert!(!include_str!("state_view.rs").contains("point_at_home"))` — cheap, and it is exactly
the discipline CD-6 found missing everywhere else.

Seeding uses the CLI (`engram remember --facts-json … --offline`), not direct `SqliteStore`
calls, so the tests exercise the same write path an agent does.

### 5.2 `tests/state_view.rs`

1. **open intervals only** — two `takes_medication` facts, one stopped (interval closed with
   `reason=stopped`, `superseded_by` NULL). `state ada` lists exactly the open one. This is the
   regression test for spec-fact-semantics risk 5: a reader that tests `superseded_by IS NULL`
   reports the stopped medication as current, and this test fails if `state` ever does that.
2. **coexistence is not conflict** — five medications, five open values in one group,
   `open_questions: []`. (Move 2's polypharmacy storm, seen from the read side.)
3. **grouping and spec fields** — `arity_class`, `safety`, `mutability`, `temporality` match
   `relation_spec` for `takes_medication`, `blood_type`, `birthday`, `lives_in`.
4. **determinism** — two runs on the same store produce byte-identical `data`.
5. **provenance chain** — every value carries `provenance.memory` (`mem:` handle),
   `provenance.chunk` (`chunk:` handle), `observed_at`, and a non-empty snippet; the memory id
   resolves through `engram memory show` (a chunk id that `edit`/`forget` reject is the exact
   defect move 5 names).
6–10. **confidence** — one test per basis term (`source_kind`, `confirmations` cap, `staleness`
   cap, `staleness == 0` for an `Immutable` relation, `contested` penalty when the fact is a side
   of an open conflict), plus the `[0.01, 0.99]` clamp. Each asserts `confidence` equals the sum
   of the published `confidence_basis` terms — the number and its stated basis can never drift.
11. **totals are not page sizes** — 30 open values, `--limit 10` → `returned: 10`, `total: 30`,
    non-null `cursor`; following the cursor yields the next 10 with no overlap and no gap.
12. **`no_match`** — unknown subject → exit **0**, `verdict: "no_match"`, `absence.top_score`,
    `absence.threshold == 0.92`, `absence.searched_kbs` naming every KB scanned,
    `absence.searched_chunks == 0`, and a `nearest_subjects` entry for the near-miss name.
12b. **fuzzy is not silent** — facts stored for "Ada Example", queried as `state "Ada Exampl"`:
    exit 0, `verdict: "results"`, `resolved_from: "fuzzy"` with a score ≥ 0.92, and
    `nearest_subjects` present in the payload. An answer about a different name than the caller
    typed must say so in the fields the caller reads (§3.2 step 9).
12c. **wildcard scope resolves** — the same near-miss under the DEFAULT `--kb '*' --diary '*'`
    still resolves and still returns a populated `nearest_subjects`. This is the regression test
    for passing the literal `"*"` into an equality-filtered query (§2.1): before the scope
    expansion rule, the default invocation was the only broken one.
13. **`--as-of`** — a medication started 2024 and stopped 2025: `--as-of 2024-06-01` shows it,
    `--as-of 2026-01-01` does not, no flag shows it not. `--as-of <now>` returns byte-identical
    `state` to no flag at all (risk 11).
13b. **point intervals survive `--as-of`** — a `birthday` (Instant, Immutable, so
    `until == Some(from)`) is present under `--as-of` at its own instant and under `--as-of` at any
    later date. A naive half-open `until > T` makes every Instant relation vanish from every
    `--as-of` query, which test 13 alone would not catch (§3.2 step 4a).
14. **`--include-closed`** — the stopped interval appears under `history` with `valid_until` and
    `close_reason`, never under `values`.
15. **scope honesty** — a fact written to `--kb work` is absent from `state ada --kb personal`
    with `verdict: "no_match"`, and present under the default `--kb '*'`; `metadata.scope.kbs`
    lists what was actually searched.

### 5.3 `tests/state_lessons.rs`

16. `state learn "<text>" --trigger fresh_write_correction` creates one lesson, exit 0, and the
    response carries `status: "confirmed"` plus an `id` — and a `handle` field ONLY if request
    X-2 granted the `lesson` handle kind; otherwise a bare uuid and no `handle` (§4.3). The test
    asserts whichever is in force, never a `lesson:` string the parser would reject. `learn`
    writes at `confirmed`
    and `reflect` writes at `proposed`: a human who typed the sentence has already confirmed it,
    a machine that proposed it has not.
17. `state lessons` lists it with `returned`/`total` and the `trigger` filter narrows correctly.
18. **dedup** — the same lesson text and trigger twice → one row, `occurrences: 2`, and the
    second call does not create a second memory twin.
18b. **a new KB does not trip the FK** — `state learn "…" --kb brandnew` into a KB that does not
    exist yet succeeds. The `lessons` DDL has `FOREIGN KEY(kb) REFERENCES knowledge_bases(name)`,
    so an implementation that upserts the lesson before `ensure_kb` fails on the very first write
    into any fresh KB (§2.4 ordering).
19. **trigger validation** — `--trigger "Not A Slug!"` → exit 3, message names the accepted shape.
20. **evidence handles** — `--evidence <bare uuid>` accepted; `--evidence mem:<uuid>` → exit 3
    `wrong_id_kind` naming `event` as the right kind; unknown event uuid → exit 3 `not_found`.
21. **the memory twin does not become a fact** — `state learn "when a caller resolves a
    takes_medication conflict, keep both"` then `engram facts list --kb '*'` shows NO
    `takes_medication` fact. Pins `no_extract_facts: true` (§2.4).
22. **the twin is recallable** — `engram recall "takes_medication conflict"` returns the lesson
    memory, tagged `kind:procedural`.
23. **confirm / retire** — status transitions land, `lesson.confirmed`/`lesson.retracted` events
    are appended, `retire` removes it from the default listing but `--status retired` finds it.
24. **retire does not delete** — the row and its events survive; `state lessons --status retired`
    plus `events_for_subject` show the full history. (Move 1: nothing is ever hard-deleted.)

### 5.4 `tests/state_reflect.rs`

Driven by `ENGRAM_REFLECT_LLM_STUB`, a scripted deterministic responder modelled exactly on
`ENGRAM_FACT_LLM_STUB` (`fact_llm.rs:25`) and `ENGRAM_COMPILER_LLM_STUB` (`compile.rs:504`), so
every reflection test is offline and deterministic.

25. **offline path produces lessons with no model** — seed a caller-overrules-machine sequence,
    run `state reflect --inline --offline`, assert ≥ 1 lesson with `rule:
    "caller_overruled_machine"` and `source_kind: "agent_extracted"`.
26. **each proposer fires** — one test per rule (`correction_within_window`, `repeat_close`,
    `retraction_after_extraction`), each asserting the rule id and the evidence event ids.
26b. **the antecedent may predate the window** — write a memory, generate ≥ `--window` unrelated
    events, THEN revise it. `correction_within_window` must still fire: its `memory.written` is
    below the window floor and is reachable only through the `events_for_subject` lookback of
    §2.5 step 2. A proposer that is pure over the window alone fails this and works only on a
    store younger than one window.
26c. **twin envelope purity** — `state learn --json` emits exactly ONE JSON document on stdout.
    `serde_json::from_str` over the whole of stdout must succeed. This is the regression test for
    calling `remember::run` (which prints) instead of `write_memory` (§2.4).
27. **idempotence / high-water mark** — running `--inline` twice over the same window writes
    zero new lessons the second time and reports the same `high_water_seq`.
28. **resumption** — new events after the first run are picked up by the second without
    re-scanning the first window (`events_scanned` is small on the second call).
29. **`--dry-run` writes nothing** — candidate list printed, `lessons` table empty, no
    `lesson.recorded` event.
30. **no key, no `--offline`** → exit 2 with a config message naming the key, never a silent
    zero-lesson success.
31. **the LLM cannot invent** — a stub response naming an unknown candidate index, changing a
    trigger, or adding evidence is discarded; the lesson written is the templated one.
32. **submitted, not inline, by default** — `state reflect` without `--inline` returns a job
    handle and does not block; `engram jobs show <id>` finds it. (Guarded by daemon-jobs' Core;
    `#[ignore]` with a named wave-6 gate if the job API lands late — see §7 R4.)
33. **isolation gate** — the `point_at_home` grep assertion of §5.1, plus an assertion that
    `std::env::var("ENGRAM_STORE")` is set in every spawned command.

### 5.5 Gates before commit (brief constraint 5)

`cargo build --workspace` · `cargo clippy --workspace -- -D warnings` · `cargo fmt --check` ·
`cargo test --workspace` · `engram bench personal --json` byte-identical to the pre-lane run
(captured into the scratchpad before the first commit, diffed after the last).

---

## 6. cross_lane_patches

Everything below is a change in a file this lane does not own. Each is written so the owning lane
or the lead can apply it without reading the rest of this spec. Grouped by owner, with the exact
anchor line as of `main` at 0.3.1 (`1d63d74`).

### 6.1 lead — `crates/engram-cli/src/commands/mod.rs`

| # | anchor | patch |
|---|---|---|
| L-1 | `mod.rs:28` (after `pub mod skill;`) | add `pub mod state;` — alphabetical, between `skill` and `update`, matching the file's existing order |
| **X-3** | `docs/arch/interfaces.md` §2, the event payload list | the three `lesson.*` payload schemas of §2.4, added to the binding contract. `EventKind` variants alone (request S-2) are not enough: export/import v3, `admin rebuild`, and any future consumer read payloads, and interfaces.md §2 is where a payload's shape is normative. A kind whose payload is documented only in a lane spec is a kind nobody else can consume |
| **X-4** | `docs/arch/interfaces.md` §2.1 | extend the migration contract to name **v10→v11** and its rule: additive-only, `CREATE TABLE IF NOT EXISTS` + indexes, no backfill, and therefore **no `VACUUM INTO` backup** — the v10 backup exists because v10 rewrites existing rows, and v11 does not touch one. §2.1 as written binds only v9→v10, and storage-journal's spec stops at `CURRENT_VERSION = 10`, so v11 currently sits outside the contract that governs migrations. This is a lead ruling, not a patch a lane can self-serve |

CD-4 flagged `commands/mod.rs` as owned by no lane; R15 assigned it to the lead.

### 6.2 contract-codegen — `cli.rs`, `agent_info.rs`, `assets/skill.md`, `respond.rs`, the schema

| # | anchor | patch |
|---|---|---|
| C-1 | `cli.rs:27-507` (`pub enum Command`) | **replaces** the placeholder `State { subject, kb, diary, relation, include_closed, as_of }` that spec-contract-codegen §4.7 pre-agreed. That shape predates the lessons/reflect surface and has no `--limit`/`--cursor`, so it cannot satisfy interfaces.md §4's `returned`/`total`/`cursor` requirement. The replacement is ONE variant, not two (C-1 and C-2 in the first draft contradicted each other — one added `State(StateCommand)`, the other needed a struct variant; the struct variant is the correct one): <br>`/// Compiled current state for a subject, plus the procedural-memory surface.`<br>`State { subject: Option<String>, #[arg(long)] subject_flag: Option<String>, #[command(subcommand)] sub: Option<StateCommand>, /* the §2.1 flags of the bare form */ }` |
| C-2 | `cli.rs` after `pub enum JobsCommand` (ends `:711`) | add `pub enum StateCommand { Lessons{…}, Learn{…}, Confirm{…}, Retire{…}, Reflect{…} }` with the flags of §2.1 — five variants, no `Show`: the bare form is `Command::State`'s own `subject` field, which is why C-1 is a struct variant. Dispatch rejects `subject` present AND `sub` present with exit 3, and `--subject` (the escape hatch of §7.1) wins over the positional |
| C-3 | `agent_info.rs:50` (after the `jobs list\|show` entry) | add the `state …` commands entry (draft text in §6.6) and the `output_data` block for `state` |
| C-4 | `assets/skill.md` | a `## Current state` section: when to prefer `state <subject>` over `facts show` (the stopped-medication trap), and that `no_match` is exit 0 |
| C-5 | schema module (`crates/engram-contract`) | the `state` command definition, so clap/agent-info/skill/JSON-Schema/contract-tests are generated from one source (move 5). This lane supplies the field list; codegen owns the generator |
| **X-1** | the epistemic-read schema (`data.absence`) | **interface addition request**: `absence` gains optional `nearest_subjects: [{subject, score}]`. Rationale: interfaces.md §4's absence shape assumes retrieval (`top_score`, `searched_chunks`); a subject-keyed read has no chunk scan, and without nearest-neighbour names an absence certificate for a misspelled name is unactionable. Additive, `#[serde(default)]`, no existing field changes meaning |
| **X-2** | `engram_core::handle` (`handle.rs:18-27`) + interfaces.md §1 grammar | **interface addition request**: a `Lesson` variant with wire prefix `lesson`. Rationale: every other addressable row in the store has a typed handle, and `state confirm <id>` is a mutator taking an id — exactly the surface move 5 gave handles to. Additive: the grammar is open at the end, bare uuids stay accepted, and no existing prefix changes. **If refused, §4.3's bare-uuid fallback ships instead** — this lane will not print a prefix the parser rejects |
| C-6 | `respond.rs` (R12) | none — `state` calls `resolve_payload` as-is. Listed so the lead knows this lane depends on R12 landing in wave 2 |

### 6.3 daemon-jobs — `lib.rs`, `jobs.rs`, `serve.rs`

| # | anchor | patch |
|---|---|---|
| D-1 | `lib.rs:13-16` (the `use crate::cli::{…}` list) | add `StateCommand` |
| D-2 | `lib.rs:296` (beside the `Command::People(sub)` arm) | add the `Command::State { subject, sub }` arm, routing to `commands::state::{run, lessons, learn, confirm, retire, reflect}`. Six match arms; every argument is already in `StateArgs`/`LessonsArgs`/`LearnArgs`/`ReflectParams` so the arm is mechanical |
| D-3 | the registry wiring named in spec-daemon-jobs §4.4 | `registry.register(Arc::new(commands::state::ReflectRunner))` — the one-liner their spec already anticipates ("`reflect` is deliberately unregistered; reasoning-layer registers it in wave 5"). `jobs list`/`show` need no change |
| **D-5** | spec-daemon-jobs §4.6 scheduler: `next_runnable_job(&["compile","backfill"])` | add `"reflect"`. **Without this D-3 is inert**: the runner is registered, `state reflect` inserts a `queued` row, and the daemon's scheduler never claims it because the kind is not in the allowlist. The job sits queued forever and test 32 asserts a handle for work that never runs. This is the patch that makes the non-`--inline` path real |
| D-4 | `serve.rs` | `GET /v1/state?subject=…&kb=…` returning `compile_state(...).to_json()` through the shared response builder — CLI/HTTP parity (move 4). **Deferrable to wave 6**; the CLI is the contract, the route is the same payload |

### 6.4 storage-journal — `engram-core/src/events.rs`, `engram-storage/src/sqlite.rs`

| # | anchor | patch |
|---|---|---|
| **S-2** | `events.rs:16-32`, `:38-54`, `:58-75`, `:79-95` | three variants — `LessonRecorded` → `"lesson.recorded"`, `LessonConfirmed` → `"lesson.confirmed"`, `LessonRetracted` → `"lesson.retracted"`; `ALL: [EventKind; 15]` becomes `[EventKind; 18]`. The existing round-trip and uniqueness tests (`events.rs:108-126`) cover the additions with no new test |
| **S-1** | `sqlite.rs` — new `SCHEMA_V11` const beside `SCHEMA_V*` (`:742` area), `CURRENT_VERSION` at `:792` → 11, migration step in `migrate()` | the `lessons` DDL of §2.4 verbatim, plus:<br>`pub fn upsert_lesson(&self, row: &LessonRow) -> Result<(Uuid, bool), StorageError>` (bool = merged rather than inserted; ON CONFLICT (kb, trigger, lesson_norm) DO UPDATE occurrences = occurrences + 1, evidence_json = union, updated_at = excluded.updated_at)<br>`pub fn get_lesson(&self, id: Uuid) -> Result<Option<LessonRow>, StorageError>`<br>`pub fn list_lessons(&self, kb: Option<&str>, trigger: Option<&str>, status: Option<&str>, limit: usize) -> Result<Vec<LessonRow>, StorageError>`<br>`pub fn count_lessons(&self, kb: Option<&str>, trigger: Option<&str>, status: Option<&str>) -> Result<i64, StorageError>` (so `total` is a real total, not a page size)<br>`pub fn set_lesson_status(&self, id: Uuid, status: &str) -> Result<bool, StorageError>`<br>`pub fn set_lesson_memory(&self, id: Uuid, memory_id: Uuid) -> Result<bool, StorageError>` |
| **S-3** | `sqlite.rs`, beside `events_for_subject` (interfaces.md §2) | `pub fn events_since(&self, kinds: &[EventKind], from_seq: i64, limit: usize) -> Result<Vec<EventRow>, StorageError>` — ordered by `seq` ASC, served by the landed `idx_events_kind`. The reflection job is a journal reader and `events_for_subject` is the wrong axis for it; without this, reflection would table-scan `events`.<br>Plus `pub fn seq_at_or_after(&self, ts: DateTime<Utc>) -> Result<i64, StorageError>` — `--since <rfc3339>` has to become a seq before the window is capped (§2.5 step 6), and only storage can do that lookup |
| **S-7** | `sqlite.rs`, beside `list_chunks_by_memory` (`:1805`) | `pub fn chunks_for_memory(&self, memory_id: Uuid) -> Result<Vec<ChunkRecord>, StorageError>` — the method the existing NAME implies. `list_chunks_by_memory(kb: Option<&str>)` lists every live chunk in a KB, so today the provenance chain for one fact costs a KB scan. Move 5 requires the chain (chunk → memory → document) on every read; this is the query that makes it O(1) |
| **S-8** | `sqlite.rs` — `open_intervals` (spec-fact-semantics §6.2) | **signature change request**: add `include_closed: bool`. `--as-of <past>` and `--include-closed` both need closed intervals across ALL relations, and `facts_for_relation` takes a REQUIRED `relation: &str`, so it cannot answer a whole-subject query. One parameter here, or a per-relation fan-out in `state.rs` (§3.2 step 4b) |
| **S-5** | `sqlite.rs` — `open_intervals` / `facts_for_relation` (spec-fact-semantics §6.2) | **signature change request**: `kb` and `diary` become `Option<&str>` (None = every KB / every diary), matching `list_facts_by_subject_scoped` (`sqlite.rs:4701`). As specified they are `&str`, so `state ada --kb '*'` would have to loop `list_kbs() × list_diaries()` and issue one query per pair — an N×M fan-out for the store's most common invocation, and a different code path from `facts list`, which already scopes with Options. One-line change at each of two signatures plus the two `(?1 IS NULL OR kb = ?1)` predicates the file already uses everywhere else |
| **S-6** | `sqlite.rs:4205` `distinct_fact_subjects(diary: &str, kb: &str)` | **signature change request**: both become `Option<&str>` (None = every diary / every KB), matching `list_facts_by_subject_scoped` (`:4701`). `state`'s DEFAULT scope is `--kb '*' --diary '*'`, and passing the literal `"*"` to an equality filter searches a KB named `*` — see §2.1. Without it, subject resolution and `nearest_subjects` fan out over `list_kbs() × diaries` on the store's most common read |
| S-4 | `admin rebuild` dispatcher | replay `lesson.*` into the `lessons` projection, so invariant 3 (byte-identical rebuild) holds with lessons present. The algorithm, so it is not re-derived: in `seq` order, `lesson.recorded` → INSERT if `lesson_id` is unseen else `occurrences += 1` and `evidence = union` (the payload carries the post-write `occurrences`, so replay SETS it rather than incrementing — that is what makes the rebuild byte-identical); `lesson.confirmed` → `status='confirmed'`, `updated_at = event.ts`; `lesson.retracted` → `status='retired'`, `updated_at = event.ts`. `memory_id` comes from the `lesson.recorded` payload; `created_at` is the first event's `ts`, never `now()` |

S-1/S-2/S-3/S-7 are larger than one-liners. They are filed here rather than implemented because
`engram-storage/**` and `engram-core/**` are storage-journal's exclusive file set (brief lane
table). Refusal consequences, each stated so nobody has to guess: **S-1** → §4.4's memory-only
degradation, `state <subject>` unaffected. **S-3** → `state reflect` BLOCKS; a full `events` scan
per run is not shippable and this lane will not add an index-free query. **S-6/S-7/S-8** →
correctness is preserved and latency is not: a fan-out over KBs (S-6), a KB chunk scan per value
(S-7), a query per relation (S-8). Those three are the difference between a read that is fast and
a read that is merely right; none of them is a reason to block.

### 6.4b write-surfaces — `commands/remember.rs`

| # | anchor | patch |
|---|---|---|
| **W-1** | `remember.rs:171` (`pub async fn run`) | extract everything before `print_success` into `pub async fn write_memory(ctx, &RememberArgs) -> Result<RememberOutcome, CliError>`; `run` becomes `write_memory` + `print_success`. Rationale in §2.4: `run` always prints (`:269`, `:568`), so any in-process caller emits a second envelope on stdout. This is a refactor with no behaviour change and one new public function. If refused, this lane writes the twin through `Core::write` directly and the two write paths drift, which is the worse outcome for write-surfaces, not for us |

### 6.5 fact-semantics — no patch, two dependencies

`engram_graph::semantics::{allowed_actions, safety_gate}` and
`engram_core::relations::{relation_spec, canonical_relation, member_identity}` must be `pub` as
their spec already writes them. `member_identity` canonicalizing internally (R6/CM-11) is
load-bearing here: `state` groups values by member and passes `--relation` through raw, so a
caller typing `--relation is_allergic_to` must land in the same group as `allergic_to`.

### 6.6 Draft agent-info text for C-3

> `state <subject> | state lessons | state learn | state reflect`: The compiled current-state
> view. `state <subject>` returns OPEN intervals only, grouped by relation, each with provenance
> (memory → chunk → document), a calibrated `confidence` and the `confidence_basis` that produced
> it, plus the open questions still queued about that subject. Prefer it over `facts show` when
> you want what is TRUE NOW: `facts show` lists a stopped medication and a current one
> identically. Flags: `--kb <name>|'*'` (default '*'), `--diary`, `--relation`, `--as-of`,
> `--include-closed`, `--limit`, `--cursor`. An unknown subject is exit **0** with
> `verdict: "no_match"` and an `absence` block naming the nearest subjects — that is an answer,
> not a failure. `state learn "<lesson>" --trigger <slug>` records a lesson about the WORK
> (procedural memory), which `recall` can then find; `state lessons` lists them;
> `state reflect` submits a background job that proposes lessons from the decision journal and
> your corrections (`--inline` to run it now, `--offline` for the no-model path). `state confirm
> <lesson-id>` promotes a proposed lesson to confirmed and makes it recallable; `state retire
> <lesson-id>` takes it out of the default listing without deleting it. `lessons`, `learn`,
> `confirm`, `retire` and `reflect` are reserved words — a subject with one of those names is
> `engram state --subject <name>`.

---

## 7. Risks and open questions

1. **`engram state lessons` is ambiguous with a subject literally named "lessons".** `state
   <subject>` with no subcommand word and `state lessons` as a subcommand cannot both be
   unambiguous in clap. The resolution in C-2: `Command::State { subject: Option<String>, sub:
   Option<StateCommand> }`, subcommand wins, and `engram state --subject lessons` is the escape
   hatch. **Open question for contract-codegen**: the alternative is `engram state show
   <subject>` with no bare form, which is unambiguous and one more word to type. This spec
   chooses the bare form because `engram state ada` is what an agent will actually write, and the
   collision set is six reserved words (`lessons`, `learn`, `confirm`, `retire`, `reflect`,
   `help`).
1b. **C-1 supersedes a shape contract-codegen already froze.** spec-contract-codegen §4.7 pins
   `State { subject: String, kb, diary, relation, include_closed, as_of }` — a bare-subject-only
   command with no lessons surface and, more consequentially, no `--limit`/`--cursor`, which
   interfaces.md §4 requires of every listing. Two specs now describe one clap variant, and an
   implementer cannot apply both. This spec's C-1/C-2 are written as the replacement, but the
   choice is not this lane's to make. **Ruling needed from the lead**: either contract-codegen
   §4.7 is amended to the §2.1 surface, or this lane drops `lessons`/`learn`/`reflect` from
   `engram state` and hangs them off a separate top-level command. Nothing else in the lane
   changes either way; it is one enum.
2. **`open_intervals` takes non-optional `kb`/`diary` (request S-5).** If S-5 is refused, `state
   --kb '*'` becomes an N×M query fan-out. That is a correctness-preserving but latency-hostile
   fallback, and on a store with ten KBs and five diaries it is fifty queries per read. The
   fallback is implementable in this lane with no other lane's help; the request exists because
   the fix belongs in one signature, not in a loop in a command file. **Honest framing**: this is
   not a one-line courtesy. Both storage-journal's spec and spec-fact-semantics §6.2 pin
   `open_intervals(kb: &str, diary: &str, …)`, and fact-semantics calls it on the write path with
   concrete scopes. S-5 and S-8 together change a signature two other specs already froze, so
   they are a **lead ruling**, not a lane-to-lane favour. Every one of S-5/S-6/S-8 is refusable
   without blocking this lane; the cost is measured in queries per read, not in correctness.
3. **`has_age` is deferred (Q3).** spec-fact-semantics §2.1 hands derivation of `has_age` from
   `birthday` + today to this lane. Shipping it in wave 5 would make one value in `state`'s
   output computed while every other value is stored, with nothing in the payload distinguishing
   them. If the lead wants it in v0.4, the shape is a separate `derived` array beside `state`,
   never a synthetic entry inside a relation group — a derived value that looks like a stored
   fact is the same lie as a chunk id that looks like a memory id.
4. **The reflect job depends on daemon-jobs' Core landing in wave 4.** `--inline` needs nothing
   from them and carries every reflection test except 32. If the job API slips, test 32 ships
   `#[ignore]`d behind a **named wave-6 integration gate**, exactly as CD-3 required for decision
   replay — and, exactly as CD-3 warns, an ignored test means the guarantee ships ungated unless
   the lead owns the gate.
5. **"Calibrated confidence" is currently *published*, not calibrated.** The five terms in §2.3
   are chosen, not measured; no dataset in this repo says a `user_stated` fact is 5 points more
   reliable than an `agent_extracted` one. The design decision is to make the number auditable
   (`confidence_basis` sums to `confidence`, tests 6–10 pin every term) so it can be recalibrated
   later against `bench personal` without changing any caller. Anyone reading `confidence` as a
   probability is over-reading it, and the field name in interfaces.md §4 invites exactly that.
   **Flag for the wave-6 external review.**
6. **Lessons can become the new 951-row backlog.** A nightly `state reflect` over a busy store
   proposes lessons faster than anyone confirms them. Mitigations in the design: the unique dedup
   index, `occurrences` instead of duplicate rows, `proposed` status excluded from the memory
   twin so unreviewed machine sentences never reach `recall`, and the high-water mark so a window
   is scanned once. Unmitigated: nothing caps the total. **Open question for the lead**: should
   `state reflect` refuse to run when `count_lessons(status='proposed')` exceeds some N (say 50),
   the way a queue with a drain problem should refuse to fill further? This spec does not
   implement a cap; it is one `count_lessons` call away if the lead wants it.
7. **The reflection LLM's output quality is not testable offline.** The stub proves the plumbing,
   the schema, and the "cannot invent" guard; it proves nothing about whether the rewritten
   sentence is a useful lesson. Same class as spec-fact-semantics risk 2. The deterministic
   proposers are the floor: if the model is bad or absent, `--offline` still produces the
   templated lesson, so the feature degrades to something honest rather than to nothing.
8. **`contested` is an N+1 risk.** Naively it is one `list_open_fact_conflicts` call per fact.
   The plan issues ONE `list_open_fact_conflicts_with_facts(diary, kb, Some(subject_norm), cap)`
   per subject and builds a `HashSet<Uuid>` of contested fact ids — the same shape `facts
   conflicts` already uses (`facts.rs` conflict path). The cap is 500; above it the field is
   omitted with `contested: null` rather than silently reported as `false`. A boolean that means
   "we did not look" is the count-meaning-page defect in miniature.
9. **The binary answers "unknown subject" three different ways.** `facts show <unknown>` exits 0
   with empty `active`/`historic` arrays and no verdict (`facts.rs:145-175`); `people show
   <unknown>` exits **3** with a `BadInput` naming the scope (`people.rs:111-120`); `state
   <unknown>` will exit 0 with `verdict: "no_match"` plus an `absence` certificate. `state`'s
   behaviour is the one interfaces.md §4 mandates, but shipping it beside two others means an
   agent that branches on exit code gets a different answer per command. Mitigation inside this
   lane: `verdict` and `absence` are both in `data` and the agent-info text says so twice.
   **Flag for the lead**: read-surfaces moving `facts show`/`people show` onto the same contract
   in wave 3 removes the inconsistency; without that, we ship three answers to one question.
10. **Lessons are scoped by `kb`, not by `diary`.** Deliberate, and it mirrors R9's decision
    scoping: diary is an authorship channel, not a namespace, and a lesson about how the store
    behaves should not need re-learning per diary. The `diary` column exists so the memory twin
    lands somewhere addressable (default `lessons`), not as a lookup key. If a caller wants
    per-diary lessons, that is a v0.5 question and the column is already there.
11. **`--as-of` in the past silently changes the query.** Step 4 of §3.2 switches from
    open-only to include-closed when `--as-of` is set. The two paths must agree for
    `--as-of == now`; test 13 asserts it. If they ever diverge, `state` has two definitions of
    "current" — the exact defect this whole lane exists to remove.
12. **`distinct_fact_subjects` filters `superseded_by IS NULL`** (`sqlite.rs:4205-4218`) — the
    very predicate risk 5 of spec-fact-semantics names as the bug, sitting inside the helper this
    lane reuses for fuzzy resolution and `nearest_subjects`. It is tolerable there and only there:
    a nearest-neighbour list is a spelling hint, not a state answer, and the state answer itself
    comes from `open_intervals`. But a subject whose every interval is closed can be missing from
    `nearest_subjects`, so an absence certificate for a person whose facts were all retired is
    less helpful than it looks. **Not fixed in this lane** — the predicate is in storage-journal's
    file and read-surfaces owns the `is_open_fact()` sweep (R15/CD-2). Flagged so the sweep's
    owner has this call site on the list.
13. **A KB delete cascades lessons out of existence with no retraction event.** The `lessons` DDL
    carries `ON DELETE CASCADE` on `kb`, matching every other projection. Under move 1 the
    `lesson.*` events survive and `admin rebuild` restores the rows, but between the delete and a
    rebuild, `state lessons` returns an empty list with no certificate saying the lessons were
    cascaded rather than never written. **Open question for the lead**: should `kb delete` append
    `lesson.retracted` for each affected lesson (making the disappearance journalled and
    explicable), or is "projections follow their KB, the journal remembers" the whole answer? The
    same question applies to every other cascading projection, which is why this spec does not
    answer it alone.
14. **Two of the four proposers are not pure over the event window.** §2.5 step 2's lookback and
    `repeat_close`'s `get_fact` resolution both reach past the window into the store. The first
    draft's promise — "pure over event rows so unit tests need no store" — held only for
    `caller_overruled_machine`. The mitigation is the `Antecedents` trait: the impurity is one
    named seam with a `HashMap` fake, not store access sprayed through the proposers. The
    alternative — widening the `fact.interval_closed` payload to carry `subject_norm`/`relation` —
    changes a binding event schema (interfaces.md §2) for one heuristic, and every existing v10
    genesis event would lack the new fields anyway. **Flagged rather than resolved**: if the lead
    prefers the payload widening, it is a better long-term shape and a worse migration.
15. **`state` and `state reflect` are one command with two risk profiles.** `state <subject>` is a
    deterministic offline read that a caller runs constantly; `state reflect` writes, calls a
    model, spends budget, and submits jobs. They share a noun because they share a subject matter,
    not because they share a blast radius. Noted for the wave-6 review: if the reserved-word
    collision of risk 1 forces a split anyway, splitting on this line rather than on clap
    convenience is the better cut.

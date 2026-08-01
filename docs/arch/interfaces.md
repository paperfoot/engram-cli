# interfaces.md v2 — the shared contract for arch/v0.4

Cross-lane ground truth, revised after coherence-report.md (67 findings; every ⚠ there is ruled
here — rulings carry the CM/CD/CW/CC/IN number they settle). Lanes code against THIS. If your
spec needs something not here, flag it in your report; do not invent a divergent shape.
Additions land only via the integration lead.

Ruling shorthand used below: **[Rn]** = lead ruling n, referenced from the changelog at the end.

## 0. Module homes and re-exports [CM-1, CM-3, CM-12, IN-5]

Fully-qualified homes. Anything shared lives in `engram-core`; `engram-graph` may re-export,
never redefine.

| Item | Home |
|---|---|
| `Handle`, `HandleKind`, `HandleParseError` | `engram_core::handle` (LANDED) |
| `EventKind` | `engram_core::events` (LANDED) |
| Relation registry (`RelationSpec`, `Arity`, `relation_spec`, `relation_arity`, `canonical_relation`, `cessation_of`, `is_safety_slot`, `member_identity`, `LegacyArity`, `predicate_arity`, versions) | `engram_core::relations` (NEW — storage-journal implements to fact-semantics' registry v1 table below; fact-semantics owns the CONTENT, storage-journal must not edit the table without a lane request) |
| Interval algebra (`Interval`, `is_open_fact`, `CloseReason`, `CloseAuthority`, `CloseVerdict`, `may_close`, `close_at`) | `engram_core::interval` (NEW — same split: storage-journal lands the file, fact-semantics owns semantics) |
| `ErrorCode` | `engram_core::error` (extend the existing module) |
| `SemanticKey`, `DecisionVerdict`, `DecisionAuthority`, `DecisionRecord`, `DecisionLedger`, classifier, planner | `engram_graph` (fact-semantics) |
| `EventDraft`, `EventRow`, `Resolved`, `DecisionRow`, `InsertOutcome` | `engram_storage` |

`engram-core/src/lib.rs` re-exports at crate root: `Handle`, `HandleKind`, `EventKind`, `Arity`,
`LegacyArity`, `RelationSpec`, `Interval`, `CloseReason`, `CloseAuthority`, `ErrorCode`.

**[R1]** One registry, one member heuristic, in `engram-core`, because the v9→v10 migration keys
14.5k decision rows via `SemanticKey` → `member_identity`, and `engram-core` cannot depend on
`engram-graph`. `engram_graph::relations` is a re-export module only.

## 1. Handles [unchanged, LANDED]

```
handle := kind ":" uuid | uuid            # bare uuid = legacy input, always accepted
kind   := mem | chunk | fact | conflict | entity | doc | job | event
```

```rust
// engram_storage
pub struct Resolved { pub kind: HandleKind, pub id: Uuid, pub memory_id: Option<Uuid> }
impl SqliteStore { pub fn resolve_handle(&self, h: &Handle) -> Result<Option<Resolved>, StorageError>; }
```

A mutator that needs a memory id accepts ANY handle: chunk → follow `memory_id`; other kind →
`wrong_id_kind` naming the actual kind and correct handle; unknown uuid → `not_found`, exit 3 —
on every mutator including `forget`. Every serialized result row carries `id` (legacy, unchanged)
plus `memory_id` and `handle` where applicable. Additive only.

## 2. The journal [IN-6, IN-12, IN-15..18]

```sql
CREATE TABLE events (
    seq        INTEGER PRIMARY KEY AUTOINCREMENT,
    id         TEXT NOT NULL UNIQUE,
    ts         TEXT NOT NULL,                       -- rfc3339, transaction time
    actor      TEXT NOT NULL,                       -- "cli:<diary>" | "daemon" | "job:<uuid>"
    kind       TEXT NOT NULL,
    subject_id TEXT,
    kb         TEXT,
    diary      TEXT,
    payload    TEXT NOT NULL                        -- canonical JSON; readers ignore unknown fields
);
CREATE INDEX idx_events_subject ON events(subject_id, seq);
CREATE INDEX idx_events_kind    ON events(kind, seq);
```

```rust
// engram_storage
pub struct EventDraft { pub kind: EventKind, pub subject_id: Option<Uuid>, pub kb: Option<String>,
                        pub diary: Option<String>, pub actor: String, pub payload: serde_json::Value }
pub struct EventRow  { pub seq: i64, pub id: Uuid, pub ts: DateTime<Utc>, pub actor: String,
                       pub kind: EventKind, pub subject_id: Option<Uuid>, pub kb: Option<String>,
                       pub diary: Option<String>, pub payload: serde_json::Value }
impl SqliteStore {
    /// Append inside the caller's open transaction.
    pub fn append_event(&self, draft: &EventDraft) -> Result<(i64, Uuid), StorageError>;
    pub fn events_for_subject(&self, id: Uuid) -> Result<Vec<EventRow>, StorageError>;
}
```

Event payloads v2 (changes vs v1 marked ←):

- `memory.written` (content, content_hash, importance, tags, event_time, source)
- `memory.revised` (memory_id, content, content_hash, prev_content_hash)
- `memory.retracted` (memory_id, reason?)
- `memory.meta_changed` (memory_id, importance?)
- `derivation.ran` (memory_id, content_hash, deriver, model?, output_fact_ids, fact_count)
- `fact.observed` (fact_id, subject, subject_norm, relation, object, object_norm, valid_from,
  valid_until?, confidence, source_memory_id, content_hash, source_kind)
- `fact.interval_closed` (fact_id, valid_until, closed_by?, reason, **authority**,
  **sets_superseded_by**) ← [R2: CM-2 — the projection columns `closed_by`/`close_reason`/
  `close_authority` and the `superseded_by` compat write must be rebuildable from this one event]
- `fact.confirmed` (fact_id)
- `conflict.opened` (conflict_id, new_fact_id, old_fact_id, detector, rule, score?,
  **decision_key**, **member_key**, **registry_version**) ← [IN-18]
- `decision.recorded` (decision_id, semantic_key, **key_version**, **verdict:
  "keep_both"|"close_old"|"reject_new"|"void"**, **authority: "caller"|"machine"**, conflict_id?,
  note?, actor, **kb**) ← [R4: CM-5]
- `decision.replayed` (decision_id, new_fact_id, old_fact_id, applied_verdict) — appended by the
  APPLIER in the same transaction as its effect; an obligation on the write path, not garnish [IN-17]
- `compile.ran` (kb, job_id?, stats)
- `job.submitted` / `job.completed` / `job.failed` (job_id, kind, params/stats/error)

Invariants:
1. Every projection mutation happens in the SAME transaction as its event append.
2. Nothing is ever deleted from `events`.
3. `engram admin rebuild [--kb K]` drops and replays projections and reproduces byte-identical
   projection state (property-tested). Owner: **storage-journal** (new `commands/admin.rs`;
   dispatch arm lands via the lead patch channel) [CD-4].
4. Export v3 = journal slice + schema version; import v3 = replay. v2 import stays supported and
   synthesizes genesis events — owner of the slice format, replay, AND v2-genesis synthesis:
   storage-journal; write-surfaces wires the commands [CD-9].

### 2.1 Migration v9→v10 contract [R5: CW-1, CC-2, CD-7 — binding; annex: spec-fact-semantics §4.1]

1. Backup BEFORE the migration transaction opens, only when stored version < 10, never for
   `open_in_memory()`: `VACUUM INTO '<data_dir>/db.sqlite.pre-v10-backup.<YYYYMMDDHHMMSS>'`
   (UTC timestamp; VACUUM INTO, not a file copy, so the WAL trio cannot be torn). **[R5a]** — the
   filename in v1 of this document and constraint 4's "repo precedent" were both wrong; there is
   no existing backup mechanism in the tree.
2. Fact backfill: `valid_until` from `superseded_at` where set, clamped
   `MAX(valid_from, superseded_at)`; `resolution` → `close_reason` and `resolved_by` → `authority`
   per the annex's mapping tables.
3. Genesis events synthesized in annex order — including `conflict.opened` for OPEN queue rows
   (without them a rebuild cannot restore the open queue and invariant 3 fails on any store with
   a backlog).
4. Resolved queue rows → `decision.recorded` events keyed by `SemanticKey::for_pair`
   (`authority` inferred per annex; unclassifiable → `verdict: "void"`).
5. Open-queue key backfill → explicit duplicate-key collapse (survivor: lowest `created_at`,
   ties by `id`; losers get `status='superseded_question'` + a `void` decision) → ONLY THEN
   `CREATE UNIQUE INDEX idx_fcq_open_key`.
6. Idempotent; `bench personal` unchanged.

## 3. Relations, intervals, facts [R1, R3, R6, R7]

### 3.1 Registry (engram_core::relations; content owned by fact-semantics)

```rust
pub enum Arity { SetValued { safety: bool }, SingleValued { safety: bool } }
pub enum Mutability { Mutable, Immutable }
pub enum Temporality { State, Instant }
pub enum ValueDomain { Date, Text }
pub struct RelationSpec { pub arity: Arity, pub mutability: Mutability,
                          pub temporality: Temporality, pub value: ValueDomain }
pub const REGISTRY_VERSION: &str = "relations@1";
pub const MEMBER_IDENTITY_VERSION: &str = "member_identity@1";
pub fn relation_spec(canonical: &str) -> RelationSpec;
pub fn relation_arity(canonical: &str) -> Arity;
pub fn canonical_relation(raw: &str) -> String;
pub fn cessation_of(raw_or_canonical: &str) -> Option<&'static str>;
pub fn is_safety_slot(canonical: &str) -> bool;
/// [R6: CM-11] Canonicalizes INTERNALLY — accepts raw or canonical; callers cannot get it wrong.
pub fn member_identity(relation: &str, object_norm: &str) -> String;
/// [R3: CM-4] The legacy projection keeps a legacy NAME at every call site. `facts.rs` et al are
/// renamed to `LegacyArity`; nothing re-exports it as `Arity`.
pub enum LegacyArity { Single, Multi }
pub fn predicate_arity(canonical: &str) -> LegacyArity;
```

Registry v1, pinned: `takes_medication`/`allergic_to`/`diagnosed_with` → `SetValued{safety:true}`,
Mutable, State. `blood_type` → `SingleValued{safety:true}`, Immutable. `birthday`/`has_birthday`/
`date_of_birth`/`born_on`/`died_on` → `SingleValued{false}`, Immutable, Instant, Date.
`nationality` → `SingleValued{false}`, Immutable, State. `lives_in`, `works_at`, `has_role`,
`has_age`, `married_to`, `partner_of`, `prefers_language` → `SingleValued{false}`, Mutable, State.
`married_on`/`founded_on`/`graduated_on`/`joined_on`/`started_on`/`ended_on`/`hired_on` →
`SetValued{false}`, Immutable, Instant, Date. Everything else, including every current
`MULTI_VALUED_PREDICATES` entry → `SetValued{safety:false}`, Mutable, State, Text.

### 3.2 Intervals (engram_core::interval)

```rust
pub struct Interval { pub from: DateTime<Utc>, pub until: Option<DateTime<Utc>> }
impl Interval { pub fn of(f: &Fact) -> Interval;
                pub fn is_open(&self) -> bool;
                pub fn overlaps(&self, other: &Interval) -> bool;   // half-open [from, until)
                pub fn precedes(&self, other: &Interval) -> bool;
                pub fn is_point(&self) -> bool; }                   // until == Some(from)
pub fn is_open_fact(f: &Fact) -> bool;   // valid_until.is_none() && superseded_by.is_none()
pub enum CloseReason { Superseded, Stopped, Corrected, RetractedSource }
pub enum CloseAuthority { ExplicitEnd, Caller { actor: String },
                          ReplayedCaller { decided_at: DateTime<Utc> },
                          SuccessorInterval, Heuristic { rule: &'static str } }
pub enum CloseVerdict { Allow, Refuse { reason: &'static str } }
pub fn may_close(spec: &RelationSpec, authority: &CloseAuthority) -> CloseVerdict;
pub fn close_at(old: &Interval, successor_from: DateTime<Utc>) -> DateTime<Utc>;  // max clamp
```

Point intervals never overlap anything; every `Instant` relation in registry v1 is `Immutable`,
so no `Instant` pair reaches the interval branch.

### 3.3 Fact projection

`Fact` gains, additive, `#[serde(default)]`: `closed_by: Option<Uuid>`,
`close_reason: Option<String>`, `close_authority: Option<String>` [IN-3]. `superseded_by`/
`superseded_at` stay populated when `close_reason='superseded'` (compat view). DDL and index
order per IN-11 of the coherence report, verbatim, including `fact_conflict_queue.status` gaining
`superseded_question` and `idx_fcq_open_key` created LAST.

Write semantics (set-valued default; contradiction = temporal incompatibility on single-valued,
or same-member overlap disagreement; safety gates interval-CLOSING and also `Duplicate`
downgrades [IN-19 note]; decision replay runs before conflict opening). Normative detail:
spec-fact-semantics.md §§1–2 as reviewed.

**[R7: CM-8/IN-25 — suggested_action ruling.]** Vocabulary stays
`keep-both | supersede-old | requires_context`. Safety pairs: ALWAYS `requires_context`, never
`supersede-old`. Non-safety single-valued pairs that queued only because autonomy was `strict`
(classifier verdict was SupersedeOld): `supersede-old` is permitted and honest. Set-valued
non-safety different members never queue at all. The blanket removal in spec-fact-semantics §1.4
is overruled; contract-codegen generates this enum.

### 3.4 Decisions [R8: CM-6, CW-3; IN-9, IN-20]

```rust
// engram_storage — owns rows, queries, precedence
pub struct DecisionRow { pub id: Uuid, pub kb: String, pub diary: String, pub decision_key: String,
                         pub subject_norm: String, pub relation: String, pub pair_hash: String,
                         pub verdict: String, pub authority: String, pub actor: String,
                         pub note: Option<String>, pub conflict_id: Option<Uuid>,
                         pub event_id: Uuid, pub key_version: String, pub decided_at: DateTime<Utc> }
impl SqliteStore {
    pub fn record_decision(&self, /* fields as DecisionRow minus id/event_id */) -> Result<Uuid, StorageError>;
    /// Precedence IN THE QUERY: newest authority='caller' row for (kb, decision_key), else newest
    /// 'machine' row; verdict='void' never selected. A failed lookup PROPAGATES — never read as
    /// "nobody decided".
    pub fn lookup_decision(&self, kb: &str, key_encoded: &str) -> Result<Option<DecisionRow>, StorageError>;
}
// engram_graph — owns the trait + semantics; the SqliteStore impl is a one-line adapter over
// lookup_decision. DecisionRow's shape above is FROZEN for that reason.
pub trait DecisionLedger { fn binding_decision(&self, kb: &str, key: &SemanticKey)
    -> Result<Option<DecisionRecord>, StorageError>; }
```

**[R9: CD-8 — pair-hash ruling.]** No new dependency. 
`object_pair_hash = Uuid::new_v5(SEMANTIC_KEY_NAMESPACE, (min(a_norm) ++ "\x1f" ++ max(b_norm)).as_bytes())`
hex-encoded, with `pub const SEMANTIC_KEY_NAMESPACE: Uuid` pinned in `engram_graph` (any fixed
uuid, chosen once). blake3 is rejected. `SEMANTIC_KEY_VERSION = "semantic_key@1"` refers to THIS
scheme. Key scoped by `kb` at lookup, deliberately not by `diary`.

### 3.5 Queue insert [R10: CM-7, IN-10]

```rust
pub enum InsertOutcome { Inserted, DuplicatePair { existing: Uuid }, DuplicateKey { existing: Uuid } }
// insert_fact_conflict gains decision_key, member_key, registry_version params and returns InsertOutcome.
```

## 4. Errors and envelope [R11: CM-10, IN-4, IN-23, IN-24]

```rust
// engram_core::error
pub enum ErrorCode { BadInput, NotFound, WrongIdKind, Config, Transient, RateLimited,
                     SqliteBusy, ConflictAlreadyResolved, MalformedJson }
```
Exit mapping: BadInput/NotFound/WrongIdKind/MalformedJson/ConflictAlreadyResolved → 3; Config → 2;
Transient/SqliteBusy → 1; RateLimited → 4.

**StorageError → ErrorCode table [R11]:** `NotFound` → NotFound; `Busy`/"database is locked" →
SqliteBusy; `Migration` → Config; `Serde`/malformed row → MalformedJson; everything else →
Transient. `error.rs` (contract-codegen) implements; a contract test asserts every variant.

Envelope additions (all additive): every listing carries `returned`, `total`, optional `cursor`
(legacy `count` stays = returned); reads carry `metadata.scope { kbs, diary, chunks_considered }`,
`data.verdict: "results" | "no_match"`, and on no_match `data.absence { top_score, threshold,
searched_kbs, searched_chunks }`; `edit`/`memory update` responses carry `facts_invalidated` and
`reextraction: "replayed"|"pending"|"none"`; `elapsed_ms` measured in dispatch (LANDED).

**[R12: CM-9]** The resolve-payload builder is a wave-2 deliverable: new module
`crates/engram-cli/src/respond.rs`, owner contract-codegen. It renders
`resolve { program, args, allowed_actions }` plus the legacy `resolve_command` string containing
ONE runnable command, no pipes. fact_write.rs, facts.rs, recall.rs, serve.rs all call it;
`allowed_actions` values arrive from the caller (fact-semantics' `allowed_actions()`), so respond.rs
depends on no classifier.

**[R13: IN-26]** `--facts-json` stamps `SourceKind::UserStated` (write-surfaces), so
caller-corrections are distinguishable from extraction.

## 5. Store resolution and test isolation [R14: CM-13, CC-1, CD-5, CD-6]

`ENGRAM_STORE` is the one name (`ENGRAM_HOME` anywhere is an error). Precedence: `--store <dir>`
flag > `ENGRAM_STORE` > ProjectDirs. The `--store` flag is DEFERRED to wave 4 (context.rs);
the env var is sufficient for waves 2–3. Tests: `point_at_home` in `tests/cli.rs` additionally
sets `ENGRAM_STORE` to the same TempDir (lead patch, wave 2); new tests use `ENGRAM_STORE` alone.
`agent-info`, `--help`, and clap errors never open, create, or migrate a store (LANDED).

## 6. Single-writer core and jobs [unchanged from v1]

`Core::write(...)` single entry point for mutations; `facts resolve` claims its row inside the
write closure (conditional UPDATE, exactly-one-row) and appends `decision.recorded`; a lost claim
is `ConflictAlreadyResolved`. Jobs API for compile/backfill/reflect. CLI and daemon share
response builders (§4 R12 lands the module early).

## 7. Ownership corrections and the patch channel [R15: CC-5, IN-29, IN-30; CD-1..CD-10]

Additions to the brief's ownership table:
- **lead (integration commits)**: `crates/engram-cli/src/commands/mod.rs`, workspace `Cargo.toml`,
  `crates/engram-cli/tests/cli.rs`, `tests/wave0_*.rs`, `tests/wave05_*.rs`, and every one-line
  cross-lane patch a lane's report requests under a `cross_lane_patches` section (e.g.
  `compile.rs:866` gains `..Default::default()` when fact-semantics lands [CD-1]).
- **storage-journal**: `crates/engram-cli/src/commands/admin.rs` (NEW, `admin rebuild`),
  `engram_core::relations` + `engram_core::interval` files (content per §3), export-v3 slice
  format + replay + v2-genesis synthesis.
- **read-surfaces**: the `is_open_fact()` sweep sites in `facts.rs:123,156,157` and
  `recall.rs:310`; storage-journal owns the fifteen `sqlite.rs` sites; the sweep is a NAMED
  wave-3 integration gate owned by the lead, with fact-semantics' §5.42 tests un-`#[ignore]`d at
  that gate [CD-2, CC-4].
- Path correction: the skill asset is `crates/engram-cli/assets/skill.md`.
- Explicitly frozen this build except via lead patches: `doctor.rs`, `repair.rs`, `chunks.rs`,
  `ingest.rs`, `kb.rs`, `reindex.rs`, `update.rs`, `usage.rs`, `config.rs`, `budget.rs`, `bench.rs`.

Already landed (do not re-spec) [CC-7]: agent-info store-free dispatch; `ENGRAM_STORE` in
`paths.rs` with peek getters; `Handle`/`EventKind` in engram-core; dispatch-level `elapsed_ms`;
bench-test isolation in `tests/cli.rs`.

## Changelog v1→v2

R1 registry+intervals in engram-core (CM-1/3/12) · R2 interval_closed carries authority +
sets_superseded_by (CM-2) · R3 LegacyArity everywhere, no alias (CM-4) · R4 decision.recorded
payload complete (CM-5) · R5 migration contract binding here, VACUUM INTO timestamped backup
(CW-1/CC-2/CD-7) · R6 member_identity canonicalizes internally (CM-11) · R7 suggested_action:
safety-only narrowing, blanket removal overruled (CM-8) · R8 DecisionRow frozen in storage,
trait adapter in graph (CM-6/CW-3) · R9 Uuid::new_v5, blake3 rejected (CD-8) · R10 InsertOutcome
(CM-7) · R11 ErrorCode + StorageError mapping (CM-10) · R12 respond.rs in wave 2 (CM-9) ·
R13 UserStated source kind (IN-26) · R14 ENGRAM_STORE everywhere, --store deferred to wave 4
(CM-13/CC-1/CD-5/CD-6) · R15 patch channel + ownership additions (CC-5/IN-29/IN-30).

## v2.1 addendum — rulings on coherence-report-v2 §7 (binding)

**D1–D28 are ADOPTED as written in coherence-report-v2.md §7**, with these clarifications:

- **D15 + D20: LANDED** (commit a35f7e8) — `engram_core::keys` with the pinned namespaces;
  `ErrorCode` wire spellings are `config_error`/`transient_error` (v0.3's exact values), serde
  matches `as_str`, `retryable()` and `ALL` included. The daemon's HTTP table uses these
  spellings (D20's table with `"config_error" => 400`).
- **D6 → option 2**: the rebuild dispatcher's rule is `memory.revised` deletes the
  `fact_extraction_log` marker for its `memory_id`. No new EventKind.
- **D2**: the three read-surfaces metadata fields (`scope.diary_applicable`, `command_alias`,
  `suggested_action_overridden`) are BLESSED; contract-codegen documents them.
- **D3 (Fact.kb)**: adopted, but LANDS in storage-journal's offset re-pinning pass (wave 2), not
  before — adding the field mid-flight breaks every constructor for no gain. `#[serde(default)]`,
  `""` on pre-v10 rows.
- **D24**: ruled ALL-IN, wave 5, one lead patch: `EventKind` gains
  `LessonRecorded|LessonConfirmed|LessonRetracted` (ALL widens 15→18) together with their payload
  schemas and `admin rebuild` arms; `HandleKind` gains `Lesson` and `Wiki` in the same commit.
  Until that commit both enums are CLOSED.
- **V10**: `crates/engram-core/src/error.rs` owner is contract-codegen from wave 2 (the landed
  baseline is lead work; extend, don't fork).
- **R7 conformance**: spec-fact-semantics.md must apply coherence-report-v2 §6's eleven edits
  verbatim plus V2–V7 before wave 3 implementation begins.
- Wave-2 GO conditions from §8 are hereby satisfied for storage-journal, contract-codegen, and
  write-surfaces' W1/W6 subset.

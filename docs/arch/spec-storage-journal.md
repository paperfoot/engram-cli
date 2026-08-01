# spec-storage-journal — implementation spec for the storage-journal lane (arch/v0.4, wave 2)

Status: spec, ready to implement **except the migration's key encoding, which is blocked on OQ-1**
(§7). Everything outside `keys.rs` and §4 steps 4–5 can start; the ~14.5k decision rows must not be
written until the lead rules where `SEMANTIC_KEY_NAMESPACE` lives, because a key written under one
home and looked up under another silently stops replaying.

Binding inputs, in precedence order:

1. `docs/arch/interfaces.md` **v2** — rulings R1–R15 override everything below and everything in
   the brief. §2 (journal + EventRow), §2.1 (migration contract), §3.1–§3.2 (registry + interval
   file content), §3.4 (decisions, `DecisionRow` FROZEN), §3.5 (`InsertOutcome`), §7 (ownership).
2. `docs/arch/redesign-brief.md` — the five moves and the seven hard constraints.
3. `docs/arch/coherence-report.md` — CM-1/2/3/6/7/11/12, CD-2/4/7/8/9/10, CW-1/3/4, CC-2/6,
   IN-1..IN-18. Every pin those raise is already ruled in interfaces v2; this spec implements the
   ruling, not the pin.
4. `docs/arch/spec-fact-semantics.md` — reviewed, owns the *semantics* of relations/intervals and
   the migration annex (§4.1) that interfaces §2.1 promotes to contract. Where its prose and
   interfaces v2 disagree, interfaces v2 wins (R5, R7 are the live examples).

This lane is **wave 2**: everything here must compile and pass on its own, before fact-semantics,
read-surfaces or write-surfaces land. Nothing here may depend on `engram-graph`.

---

## 1. Scope and non-goals

### 1.1 In scope (this lane implements)

| # | Deliverable | Home |
|---|---|---|
| D1 | `events` table + `EventDraft`/`EventRow`/`append_event`/`events_for_subject` | `crates/engram-storage/src/journal.rs` (NEW), `sqlite.rs` |
| D2 | Projection maintenance in the same transaction as the event append (invariant 1) | `journal.rs` write helpers |
| D3 | `engram admin rebuild [--kb K]` — drop + replay projections, byte-identical (invariant 3) | `crates/engram-cli/src/commands/admin.rs` (NEW) |
| D4 | Migration **v9 → v10** per the binding contract, incl. `VACUUM INTO` backup, genesis synthesis, duplicate-key collapse, `conflict.opened` for OPEN rows | `sqlite.rs` (`migrate_v10`), `backup.rs` (NEW) |
| D5 | Derivation ledger keyed `(content_hash, extractor_id@version)` | `sqlite.rs` + v10 DDL |
| D6 | Decision ledger: `fact_decisions` DDL, `DecisionRow`, `record_decision`, `lookup_decision` with **precedence in the SQL** | `sqlite.rs` |
| D7 | Handle resolution: `Resolved`, `SqliteStore::resolve_handle` | `sqlite.rs` |
| D8 | `engram_core::relations` — **LANDED** (see §2.1); remaining work is the conformance check against interfaces v2 §3.1 and the wave-3 handover to fact-semantics | `crates/engram-core/src/relations.rs` |
| D9 | `engram_core::interval` — **LANDED** (see §2.1); same | `crates/engram-core/src/interval.rs` |
| D8b | `engram_core::keys` — the dependency-free hash primitives R9 pins, so the migration and the classifier share one key encoding | `crates/engram-core/src/keys.rs` (NEW) |
| D10 | `Fact` gains `closed_by` / `close_reason` / `close_authority` (additive, `#[serde(default)]`) | `crates/engram-core/src/types.rs` |
| D11 | Interval reads/writes: `facts_for_relation`, `open_intervals`, `close_fact_interval`, `supersede_fact_with` re-expressed without a hard-coded authority | `sqlite.rs` |
| D12 | Queue insert widened: `decision_key`/`member_key`/`registry_version` params, `InsertOutcome` return | `sqlite.rs` |
| D13 | The fourteen `superseded_by IS NULL` query sites in `sqlite.rs` migrated to open-interval semantics | `sqlite.rs` |
| D14 | Export **v3** journal slice + replay import + **v2-genesis synthesis** | `crates/engram-storage/src/export_v3.rs` (NEW) |
| D15 | `StorageError` variants sufficient for the R11 mapping table | `crates/engram-storage/src/error.rs` |

### 1.2 Explicit non-goals (other lanes, or later waves)

- **Semantics of relations and intervals.** D8/D9 land *files* whose content is transcribed from
  interfaces v2 §3.1–§3.2. fact-semantics owns the table's meaning; this lane must not add,
  remove or re-classify a relation without a lane request through the lead.
- `classify_pair`, `safety_gate`, `SemanticKey`, `DecisionRecord`, `DecisionLedger`, `replay`,
  `plan_write` — fact-semantics, `engram-graph` (interfaces §0). This lane provides the *rows*
  and the *query*; the trait impl is a one-line adapter written in `engram-graph`.
- The **CLI dispatch arm** for `admin` (`lib.rs`) and `pub mod admin;` (`commands/mod.rs`) —
  lead patch channel, §7 below.
- `commands/export.rs` / `import.rs` wiring — write-surfaces (CD-9); this lane supplies the
  format, the writer and the replayer as library functions plus their tests.
- `error.rs` in engram-cli, `ErrorCode`, the envelope, `respond.rs` — contract-codegen.
- `--store` flag plumbing (`context.rs`, `cli.rs`) — deferred to wave 4 by R14. The env var
  `ENGRAM_STORE` is already landed and is sufficient.
- `facts.rs:123,156,157` and `recall.rs:310` open-fact sweep sites — read-surfaces (R15/CD-2).
  This lane owns only the fourteen `sqlite.rs` query sites and hands read-surfaces a helper.
- Anything already landed: `Handle`/`HandleKind`/`HandleParseError` (`engram-core/src/handle.rs`),
  `EventKind` (`engram-core/src/events.rs`, 15 kinds, `ALL`, `as_str`/`parse`), `ENGRAM_STORE` +
  peek getters (`engram-storage/src/paths.rs`), agent-info store-free dispatch, dispatch
  `elapsed_ms`. Read them; do not re-spec or re-derive them.
- Landed since interfaces v2 and verified in the tree while this spec was written:
  `engram-core/src/relations.rs` (registry v1, `member_identity`, `LegacyArity`,
  `predicate_arity`, both version consts), `engram-core/src/interval.rs` (`Interval`,
  `is_open_fact`, `CloseReason`/`CloseAuthority` **with `as_str()`**, `CloseVerdict`, `may_close`,
  `close_at`), `engram_core::error::ErrorCode` with `exit_code()`, and the
  `engram-core/src/lib.rs` re-export list. This lane does **not** rewrite them; §2.1 lists the
  conformance checks it owns instead.

### 1.3 Constraints this lane is judged against

- **ENGRAM_STORE TempDir in every test.** No `HOME` tricks in new tests (R14, CC-1). A test that
  can open `~/Library/Application Support/bio.199-biotechnologies.engram/` is an automatic reject.
- No `cargo install`, no migration run against any store outside a TempDir.
- `cargo build --workspace` / `clippy -D warnings` / `fmt --check` / `test --workspace` green
  before each commit; `engram bench personal --json` unchanged.
- Additive JSON only. Existing field names and types survive.
- Never edit another lane's file; one-liners go through §6 `cross_lane_patches`.

---

## 2. Data model and API changes

### 2.1 `engram-core` — two landed modules to verify, one new, one additive struct change

`relations.rs` and `interval.rs` are already in the tree (verified while this spec was written,
including `CloseReason::as_str()` / `CloseAuthority::as_str()`, which are not in interfaces v2 but
are exactly what the storage columns need). `keys.rs` is new. `engram-core` takes no new
dependency: `uuid` is already vendored with `v5` (workspace `Cargo.toml:81`), which is what R9
relies on.

**`crates/engram-core/src/relations.rs` — LANDED. This lane verifies, it does not rewrite.**
The file exists with the shape below. The lane's remaining obligation is a conformance test
(§5.2 tests 1-6) plus the wave-3 handover note: fact-semantics owns the CONTENT of the table and
may change it through a lane request; storage-journal may not.

```rust
pub const REGISTRY_VERSION: &str = "relations@1";
pub const MEMBER_IDENTITY_VERSION: &str = "member_identity@1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arity { SetValued { safety: bool }, SingleValued { safety: bool } }
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mutability { Mutable, Immutable }
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Temporality { State, Instant }
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueDomain { Date, Text }
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RelationSpec { pub arity: Arity, pub mutability: Mutability,
                          pub temporality: Temporality, pub value: ValueDomain }
impl RelationSpec {
    pub fn is_safety(&self) -> bool;
    pub fn is_set_valued(&self) -> bool;
    pub fn is_single_valued(&self) -> bool;
}
pub fn relation_spec(canonical: &str) -> RelationSpec;
pub fn relation_arity(canonical: &str) -> Arity;      // = relation_spec(c).arity
pub fn canonical_relation(raw: &str) -> String;
pub fn cessation_of(raw_or_canonical: &str) -> Option<&'static str>;
pub fn is_safety_slot(canonical: &str) -> bool;       // = relation_spec(c).is_safety()
/// [R6] Canonicalizes INTERNALLY: `member_identity("is_allergic_to", x)` and
/// `member_identity("allergic_to", x)` return the same member. The parameter is
/// named `relation`, not `canonical_relation`, because the caller cannot get it wrong.
pub fn member_identity(relation: &str, object_norm: &str) -> String;
/// [R3] The legacy JSON projection. NOTHING re-exports this as `Arity`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LegacyArity { Single, Multi }
pub fn predicate_arity(canonical: &str) -> LegacyArity;
```

Conformance checks this lane owns, because a drift here is a silent safety regression and nothing
else in wave 2 would catch it:

1. Every relation pinned in interfaces v2 §3.1 resolves to its stated `RelationSpec` (test 1).
2. Anything unlisted defaults to `SetValued { safety: false }`, `Mutable`, `State`, `Text` — the
   safe direction (test 2).
3. `member_identity` canonicalizes internally [R6] (test 3) and matches the v1 algorithm in
   `spec-fact-semantics.md` §2.1 lines 199-213 (test 4).
4. `predicate_arity("allergic_to") == LegacyArity::Multi` — the polypharmacy defect as an
   assertion (test 5).
5. No registry entry is `Instant + Mutable`, the assumption `interval.rs`'s point algebra rests
   on (test 6).

The legacy tables in `crates/engram-cli/src/fact_write.rs:89-151` (`MULTI_VALUED_PREDICATES`,
`slot_policy`) and `crates/engram-graph/src/facts.rs` (`canonical_predicate`) are **still live**
and are fact-semantics' to delete in wave 3. Two tables for one wave is deliberate: it is what
keeps the workspace green between waves. This lane does not touch either.

**`crates/engram-core/src/interval.rs` — LANDED.** Verified present with the pinned signatures,
plus `CloseReason::as_str()` and `CloseAuthority::as_str()`, which are the canonical spellings of
the `close_reason` / `close_authority` columns and of the `fact.interval_closed` payload fields —
storage uses them rather than string literals, so the column vocabulary has one source. Behaviour
this lane depends on and tests (7-10):

- `Interval::of(f: &Fact)` — `from = valid_from.or(created_at)`, `until = valid_until.or(superseded_at when superseded_by.is_some())`. Never panics on a NULL `valid_from` (a hand-restored v9 backup).
- `overlaps` is half-open `[from, until)`; a point interval (`until == Some(from)`) overlaps nothing, including itself.
- `may_close(spec, authority)` — a safety relation admits only `ExplicitEnd`, `Caller`, `ReplayedCaller`; `SuccessorInterval` and `Heuristic` are `Refuse`. An `Immutable` relation additionally refuses `SuccessorInterval`.
- `close_at(old, successor_from) = max(old.from, successor_from)` — the same clamp the migration applies in SQL (§4 step 2). One clamp, two expressions, one test that pins them equal.

**`crates/engram-core/src/keys.rs` (NEW, ~120 lines) — see OQ-1.** The dependency-free hash
primitives R9 pins, placed where both `engram-storage` (the migration) and `engram-graph` (the
classifier) can reach them:

```rust
/// Fixed once, forever: it is baked into every migrated decision key. Any
/// fixed uuid is legal [R9]; this one was chosen here so nobody has to choose
/// again. Use `Uuid::from_u128(0x6d1b9d3a4f0e5a7c9b218f1c0d6e4a55)` if the
/// `uuid!` macro is not in scope — same value, no feature question.
pub const SEMANTIC_KEY_NAMESPACE: Uuid = uuid::uuid!("6d1b9d3a-4f0e-5a7c-9b21-8f1c0d6e4a55");
pub const SEMANTIC_KEY_VERSION: &str = "semantic_key@1";
pub const CONTENT_HASH_NAMESPACE: Uuid = uuid::uuid!("2f7c5e88-1a34-5c62-8d90-3b4e7a1f6c02");

/// [R9] hex(uuid5(NS, min ++ 0x1f ++ max)) over the two facts' `object_norm`s,
/// so the key is the same whichever fact arrived first. blake3 is rejected.
pub fn object_pair_hash(a_norm: &str, b_norm: &str) -> String;
/// "<subject_norm>\u{1f}<relation>\u{1f}<object_pair_hash>"
pub fn semantic_key_encode(subject_norm: &str, relation: &str, pair_hash: &str) -> String;
pub fn semantic_key_decode(s: &str) -> Option<(String, String, String)>;
/// Content addressing for the derivation ledger: hex(uuid5(CONTENT_NS, content)).
pub fn content_hash(content: &str) -> String;
```

`engram_graph::SemanticKey` (fact-semantics) keeps its struct and its
`for_pair(&FactView, &FactView)` constructor and delegates to these two functions, so there is
exactly one key encoding in the workspace. The `relation` component is
`canonical_relation(predicate)`; the pair components are the two `object_norm`s (per
`spec-fact-semantics.md` §2.3.2 line 465), **not** the member identities.

**`crates/engram-core/src/types.rs` — `Fact` gains three fields [IN-3, CD-10]**, all
`#[serde(default)]`, appended after `confirmations` so field order in existing JSON is unchanged:

```rust
    /// The successor fact (or the event) that closed this interval.
    #[serde(default)] pub closed_by: Option<Uuid>,
    /// superseded | stopped | corrected | retracted_source
    #[serde(default)] pub close_reason: Option<String>,
    /// explicit_end | caller | replayed_caller | successor_interval | heuristic
    /// — verbatim `CloseAuthority::as_str()` (`interval.rs:131-141`). The
    /// landed spelling is `successor_interval`, NOT `successor`; nothing in
    /// this lane writes the short form.
    #[serde(default)] pub close_authority: Option<String>,
```

Every `Fact { .. }` literal in the workspace must gain the three fields or `..Default::default()`.
`Fact` has no `Default` impl today and gains none (too many required ids); the literal sites are
enumerated in §6 `cross_lane_patches` (CLP-7).

**`crates/engram-core/src/lib.rs` [IN-5]** — the interfaces v2 §0 re-export list is **already
landed verbatim** (`lib.rs:6-22`, including `pub mod interval; pub mod relations;` and
`pub use error::{CoreError, ErrorCode};`). This lane's whole delta to that file is two lines:

```rust
pub mod keys;                       // new module, §2.1
pub use keys::SEMANTIC_KEY_VERSION; // only if OQ-1 rules keys.rs into engram-core
```

Do **not** rewrite the existing `pub use` block; dropping the landed `ErrorCode` re-export would
regress IN-5 and break contract-codegen.

`ErrorCode` (interfaces §4) is **already landed** in `engram_core::error` with `exit_code()`, and
belongs to contract-codegen. This lane's only obligation towards it is §2.10: make `StorageError`
carry enough structure for `engram-cli`'s mapping to be a total function.

### 2.2 The journal — `crates/engram-storage/src/journal.rs` (NEW)

DDL, appended to the v10 migration (§4) and to `SCHEMA_V1`-equivalent fresh-store creation:

```sql
CREATE TABLE IF NOT EXISTS events (
    seq        INTEGER PRIMARY KEY AUTOINCREMENT,
    id         TEXT NOT NULL UNIQUE,
    ts         TEXT NOT NULL,      -- rfc3339 UTC, transaction time
    actor      TEXT NOT NULL,      -- "cli:<diary>" | "daemon" | "job:<uuid>" | "migration:v10" | "import:v2"
    kind       TEXT NOT NULL,
    subject_id TEXT,
    kb         TEXT,
    diary      TEXT,
    payload    TEXT NOT NULL       -- canonical JSON; readers ignore unknown fields
);
CREATE INDEX IF NOT EXISTS idx_events_subject ON events(subject_id, seq);
CREATE INDEX IF NOT EXISTS idx_events_kind    ON events(kind, seq);
CREATE INDEX IF NOT EXISTS idx_events_kb_seq  ON events(kb, seq);   -- rebuild --kb, export slice
```

`actor` extends interfaces v2 §2's grammar (`"cli:<diary>" | "daemon" | "job:<uuid>"`) with two
synthesis actors, `migration:v10` and `import:v2`. It is a free-text column and nothing validates
it as an enum today, but the extension is reported to the lead for interfaces v2.1 alongside OQ-2:
a later validator that treats the grammar as closed would reject every genesis event in every
migrated store.

`events` carries **no foreign keys**. That is the point of move 1: a memory row can be retracted,
a fact can be re-derived under a new uuid, and the journal that records both must survive them.
The FK CASCADE on `fact_conflict_queue` (`sqlite.rs:1106-1108`) is what erased 14.5k adjudication
rows on one `edit --content`; nothing new in this lane may reintroduce that pattern.

```rust
// crates/engram-storage/src/journal.rs
#[derive(Debug, Clone)]
pub struct EventDraft {
    pub kind: EventKind, pub subject_id: Option<Uuid>, pub kb: Option<String>,
    pub diary: Option<String>, pub actor: String, pub payload: serde_json::Value,
}
#[derive(Debug, Clone)]
pub struct EventRow {
    pub seq: i64, pub id: Uuid, pub ts: DateTime<Utc>, pub actor: String,
    pub kind: EventKind, pub subject_id: Option<Uuid>, pub kb: Option<String>,
    pub diary: Option<String>, pub payload: serde_json::Value,
}
impl EventDraft {
    /// Builder used by every projection writer, so `actor` is never forgotten.
    pub fn new(kind: EventKind, actor: impl Into<String>, payload: serde_json::Value) -> Self;
    pub fn subject(self, id: Uuid) -> Self;
    pub fn scope(self, kb: Option<&str>, diary: Option<&str>) -> Self;
}
impl SqliteStore {
    /// Append inside the caller's OPEN transaction. Never opens one itself:
    /// invariant 1 (projection mutation and event append are one transaction)
    /// is a property of the caller's `with_tx`, and a self-opening append
    /// would quietly commit an event whose effect later rolled back.
    pub fn append_event(&self, draft: &EventDraft) -> Result<(i64, Uuid), StorageError>;
    /// The identity-preserving sibling, and the ONLY way genesis synthesis
    /// (§4.3) and v3 import (§2.13) can work: `append_event` mints a fresh
    /// `Uuid::new_v4()` and stamps `Utc::now()`, which would make the
    /// migration non-idempotent (no deterministic id to `INSERT OR IGNORE`
    /// against) and would erase an imported journal's original timestamps.
    /// `INSERT OR IGNORE`; `Ok(None)` when the id was already present.
    pub fn append_event_with_identity(&self, id: Uuid, ts: DateTime<Utc>, draft: &EventDraft)
        -> Result<Option<i64>, StorageError>;
    pub fn events_for_subject(&self, id: Uuid) -> Result<Vec<EventRow>, StorageError>;
    /// Ordered journal slice for rebuild, export and `admin rebuild --kb`.
    pub fn events_slice(&self, after_seq: i64, kb: Option<&str>, limit: Option<usize>)
        -> Result<Vec<EventRow>, StorageError>;
    pub fn event_by_id(&self, id: Uuid) -> Result<Option<EventRow>, StorageError>;
    pub fn max_event_seq(&self) -> Result<i64, StorageError>;
}
/// Deterministic serialization: object keys sorted recursively, no whitespace.
/// `admin rebuild` must reproduce byte-identical payloads, and that guarantee
/// cannot rest on a serde_json feature flag another crate might switch on.
pub fn canonical_json(v: &serde_json::Value) -> String;
```

`append_event` generates `id = Uuid::new_v4()`, stamps `ts = Utc::now().to_rfc3339()`, serializes
via `canonical_json`, and carries
`debug_assert!(!self.conn.is_autocommit(), "append_event outside a transaction violates journal invariant 1")`
— a lane-discipline tripwire that costs nothing in release. It is a tripwire and not the guarantee:
in release an `append_event` outside a transaction autocommits an event whose effect may still
fail. The guarantee is that every method in §2.3's table opens its own `with_tx`, which joins an
already-open transaction (`sqlite.rs:834-836`) rather than nesting — so `compile`'s explicit
`begin_immediate`/`commit` pattern composes with journaling writes without either side knowing.
`insert_memory_with_kb`, `insert_fact` and `update_memory` are bare `execute` calls today and each
gains a `with_tx` wrapper as part of this lane. `SqliteStore.conn` becomes
`pub(crate)` (one-word change in this lane's own `sqlite.rs:26`) so `journal.rs`, `backup.rs` and
`export_v3.rs` can prepare statements.

Payload schemas are interfaces v2 §2 verbatim. Payload construction lives in one place —
`journal::payload::{memory_written, fact_observed, fact_interval_closed, conflict_opened,
decision_recorded, decision_replayed, derivation_ran, …}` — so the migration's synthesized
genesis events and the live write path emit byte-identical shapes. That is not tidiness: it is
what makes the migration's genesis events replayable by the same rebuild dispatcher.

`kind` is stored as `EventKind::as_str()` and read back with `EventKind::parse`; an unparseable
kind on read is `StorageError::MalformedRow`, never a silent skip, because a skipped event is a
projection that silently diverges from its journal.

### 2.3 Projection maintenance — which write appends which event

Invariant 1 in table form. Every row is a `with_tx` closure containing both halves.

| storage method (`sqlite.rs`) | event appended | subject_id |
|---|---|---|
| `insert_memory_with_kb:1457` | `memory.written` | memory id |
| `update_memory:1897` (content changed) | `memory.revised` | memory id |
| `update_memory:1897` (importance only) | `memory.meta_changed` | memory id |
| `soft_delete_memory:1855` | `memory.retracted` | memory id |
| `record_derivation` (NEW, wraps `record_fact_extraction:1971`) | `derivation.ran` | memory id |
| `insert_fact:3937` | `fact.observed` | fact id |
| `close_fact_interval` (NEW) / `supersede_fact_with:4105` | `fact.interval_closed` | fact id |
| `increment_fact_confirmations:4175` | `fact.confirmed` | fact id |
| `insert_fact_conflict:4229` (outcome `Inserted`) | `conflict.opened` | conflict id |
| `record_decision` (NEW) | `decision.recorded` | conflict id when known, else decision id |
| applier-side replay (fact-semantics calls `record_replay`, NEW) | `decision.replayed` | new fact id |

`decision.replayed` [IN-17] is an obligation on the write path, so this lane ships the append
helper `record_replay(&self, kb, decision_id, new_fact_id, old_fact_id, applied_verdict, actor)`;
fact-semantics' `apply_plan` calls it once per applied replay in the same transaction as the
effect. Without the helper the obligation is prose.

**Non-goal, stated so nobody widens the wave:** `insert_chunk*`, entity/relation upserts, wiki
pages and claims append **no** events in wave 2. They are compiler caches, regenerated by
`compile`; `admin rebuild` restores them by re-running compile, not by replay. `compile.ran` is
appended by daemon-jobs in wave 4.

### 2.4 Derivation ledger, keyed `(content_hash, extractor_id@version)`

```sql
CREATE TABLE IF NOT EXISTS derivations (
    content_hash    TEXT NOT NULL,
    extractor_id    TEXT NOT NULL,          -- "<name>@<version>", e.g. "fact_llm@1"
    memory_id       TEXT NOT NULL,
    derived_at      TEXT NOT NULL,
    model           TEXT,
    fact_count      INTEGER NOT NULL DEFAULT 0,
    output_fact_ids TEXT NOT NULL DEFAULT '[]',   -- JSON array of fact uuids
    event_id        TEXT NOT NULL,
    PRIMARY KEY (content_hash, extractor_id)
);
CREATE INDEX IF NOT EXISTS idx_derivations_memory ON derivations(memory_id);
```

```rust
pub struct DerivationRow { pub content_hash: String, pub extractor_id: String,
                           pub memory_id: Uuid, pub derived_at: DateTime<Utc>,
                           pub model: Option<String>, pub fact_count: i64,
                           pub output_fact_ids: Vec<Uuid>, pub event_id: Uuid }
impl SqliteStore {
    /// Upsert + `derivation.ran`, in one transaction. Also writes the legacy
    /// `fact_extraction_log` marker so the backfill work queue keeps working.
    pub fn record_derivation(&self, memory_id: Uuid, content_hash: &str, extractor_id: &str,
                             model: Option<&str>, output_fact_ids: &[Uuid], actor: &str)
        -> Result<(), StorageError>;
    /// Memoization lookup: "has this exact content already been derived by this
    /// exact extractor version?" — content-addressed, so an edit that restores
    /// previous content is a cache HIT and re-extraction is skipped.
    pub fn derivation_for(&self, content_hash: &str, extractor_id: &str)
        -> Result<Option<DerivationRow>, StorageError>;
    pub fn derivations_for_memory(&self, memory_id: Uuid)
        -> Result<Vec<DerivationRow>, StorageError>;
}
```

`content_hash` is `engram_core::keys::content_hash(&memory.content)`. `extractor_id` is the
caller's string; this lane pins the two in use today as constants so nobody spells them twice:
`pub const EXTRACTOR_FACT_LLM: &str = "fact_llm@1";` and
`pub const EXTRACTOR_COMPILE: &str = "compile_llm@1";` in `journal.rs`.

`fact_extraction_log` (`sqlite.rs:742`, memory-keyed) is **kept** and still drives
`PENDING_EXTRACTION_PREDICATE:735`. `derivations` is content-keyed and additive. The v10
migration backfills one `derivations` row per `fact_extraction_log` row (§4 step 3). The brief's
"backfill pending predicate is marker-only, contradicting agent-info" defect is a read/daemon
surface fix and is NOT closed here — this lane supplies the content-addressed table it needs.

### 2.5 Decision ledger — rows, queries, precedence IN the SQL [R8, IN-9]

DDL exactly as IN-11 pins it (§4 step 1 creates it):

```sql
CREATE TABLE IF NOT EXISTS fact_decisions (
    id TEXT PRIMARY KEY, kb TEXT NOT NULL DEFAULT 'default',
    diary TEXT NOT NULL DEFAULT 'default', decision_key TEXT NOT NULL,
    subject_norm TEXT NOT NULL, relation TEXT NOT NULL, pair_hash TEXT NOT NULL,
    verdict TEXT NOT NULL, authority TEXT NOT NULL, actor TEXT NOT NULL, note TEXT,
    conflict_id TEXT, event_id TEXT NOT NULL, key_version TEXT NOT NULL, decided_at TEXT NOT NULL);
CREATE INDEX IF NOT EXISTS idx_fact_decisions_key
    ON fact_decisions(kb, decision_key, authority, decided_at DESC);
```

`DecisionRow` is **frozen** (interfaces v2 §3.4) because `engram-graph` implements
`DecisionLedger for SqliteStore` against it:

```rust
pub struct DecisionRow { pub id: Uuid, pub kb: String, pub diary: String, pub decision_key: String,
                         pub subject_norm: String, pub relation: String, pub pair_hash: String,
                         pub verdict: String, pub authority: String, pub actor: String,
                         pub note: Option<String>, pub conflict_id: Option<Uuid>,
                         pub event_id: Uuid, pub key_version: String, pub decided_at: DateTime<Utc> }

impl SqliteStore {
    #[allow(clippy::too_many_arguments)]
    pub fn record_decision(&self, kb: &str, diary: &str, key_encoded: &str, verdict: &str,
                           authority: &str, actor: &str, note: Option<&str>,
                           conflict_id: Option<Uuid>, key_version: &str)
        -> Result<Uuid, StorageError>;
    pub fn lookup_decision(&self, kb: &str, key_encoded: &str)
        -> Result<Option<DecisionRow>, StorageError>;
    pub fn decisions_for_key(&self, kb: &str, key_encoded: &str)
        -> Result<Vec<DecisionRow>, StorageError>;   // history, newest first; includes void
}
```

`record_decision` decomposes `key_encoded` with `keys::semantic_key_decode` into
`subject_norm`/`relation`/`pair_hash` (a malformed key is `StorageError::MalformedRow`, never a
silently blank column), appends `decision.recorded` with the full R4 payload
(`decision_id, semantic_key, key_version, verdict, authority, conflict_id?, note?, actor, kb`),
inserts the projection row with `event_id` = the appended event, and returns the decision id.
Both halves in one `with_tx`.

**The precedence is one statement, and the caller never sees a choice** [R8, IN-9]:

```sql
SELECT id, kb, diary, decision_key, subject_norm, relation, pair_hash, verdict, authority,
       actor, note, conflict_id, event_id, key_version, decided_at
FROM fact_decisions
WHERE kb = ?1 AND decision_key = ?2 AND verdict <> 'void'
ORDER BY (authority = 'caller') DESC, decided_at DESC, id DESC
LIMIT 1
```

`(authority = 'caller') DESC` first is the whole rule: the newest caller row beats a newer
machine row, `void` is never selected, and `id DESC` makes ties deterministic so the byte-identical
rebuild property is not decided by row order. Scoped by `kb`, deliberately not by `diary` [R9].
A failed lookup returns `Err` and **propagates** — `lookup_decision` never maps an error to
`Ok(None)`, because "the database was busy" read as "nobody has decided this" re-asks a settled
safety question and can re-close an interval the caller reinstated.

### 2.6 Queue insert with an unambiguous negative result [R10, IN-10]

```sql
ALTER TABLE fact_conflict_queue ADD COLUMN decision_key     TEXT;
ALTER TABLE fact_conflict_queue ADD COLUMN member_key       TEXT;
ALTER TABLE fact_conflict_queue ADD COLUMN registry_version TEXT;
-- created LAST, after the §4 step 5.2 collapse:
CREATE UNIQUE INDEX IF NOT EXISTS idx_fcq_open_key
    ON fact_conflict_queue(kb, decision_key) WHERE status='open' AND decision_key IS NOT NULL;
```

```rust
pub enum InsertOutcome { Inserted, DuplicatePair { existing: Uuid }, DuplicateKey { existing: Uuid } }

impl SqliteStore {
    #[allow(clippy::too_many_arguments)]
    pub fn insert_fact_conflict(&self, id: Uuid, new_fact_id: Uuid, old_fact_id: Uuid,
                                diary: &str, kb: &str, detector: &str, score: Option<f32>,
                                suggested: &str, decision_key: &str, member_key: &str,
                                registry_version: &str) -> Result<InsertOutcome, StorageError>;
}
```

Implementation order inside the caller's `BEGIN IMMEDIATE` (so the pre-checks cannot race):

1. `SELECT id FROM fact_conflict_queue WHERE new_fact_id=?1 AND old_fact_id=?2` → `DuplicatePair`.
   Same-pair semantics are unchanged from the existing `UNIQUE(new_fact_id, old_fact_id)` at
   `sqlite.rs:1106`: the pair is ORDERED, and this lane does not widen it — widening it silently
   changes which conflicts get queued and belongs to fact-semantics.
2. `SELECT id FROM fact_conflict_queue WHERE kb=?1 AND decision_key=?2 AND status='open'` →
   `DuplicateKey`.
3. Otherwise plain `INSERT` (**not** `INSERT OR IGNORE` — an ignored insert is exactly the
   ambiguity CM-7 is about) + `conflict.opened` with the IN-18 payload
   (`conflict_id, new_fact_id, old_fact_id, detector, rule, score?, decision_key, member_key,
   registry_version`) → `Inserted`.
4. A constraint violation surviving steps 1-2 is re-queried once and mapped to whichever
   duplicate it was; if neither query finds a row, the error propagates. Never `Ok(Inserted)`.

`fact_conflict_queue.status` gains the value `superseded_question` — used by the §4 step 5.2
collapse and by the write path when a new pair collides with an open question that already carries
its key. It is not `resolved` (nobody answered it) and every `status='open'` listing ignores it.

### 2.7 Handle resolution [IN-7]

```rust
pub struct Resolved { pub kind: HandleKind, pub id: Uuid, pub memory_id: Option<Uuid> }
impl SqliteStore {
    pub fn resolve_handle(&self, h: &Handle) -> Result<Option<Resolved>, StorageError>;
}
```

- `h.kind == Some(k)`: probe only `k`'s table. A uuid that exists under a different kind still
  returns `None` here; the *caller* then calls `resolve_handle(&Handle { kind: None, .. })` to
  name the actual kind in a `wrong_id_kind` error. Both steps are cheap and the split keeps the
  storage API free of CLI error vocabulary.
- `h.kind == None` (legacy bare uuid): probe in a **pinned** order — `memories`, `chunks`,
  `facts`, `fact_conflict_queue`, `entities`, `documents`, `compile_jobs`, `events` — and return
  the first hit. The order is pinned so two stores with the same rows answer identically; uuid
  collisions across tables are a practical impossibility and a deterministic answer beats a
  clever one.
- `memory_id` is populated for `Memory` (itself), `Chunk` (`chunks.memory_id`) and `Fact`
  (`facts.source_memory_id`); `None` otherwise.
- **Soft-deleted memories resolve.** `forget`/`edit` must be able to say "already deleted"
  instead of "not found" — the brief's `forget.rs:17-21` exit-0-success-deleted-false defect and
  the "no memory found … (already deleted?)" guess both come from conflating the two. `Resolved`
  carries no `deleted` flag; callers that care read the memory row.

### 2.8 Interval reads and writes [IN-8, CW-4]

```rust
impl SqliteStore {
    pub fn facts_for_relation(&self, kb: &str, diary: &str, subject_norm: &str, relation: &str,
                              include_closed: bool) -> Result<Vec<Fact>, StorageError>;
    pub fn open_intervals(&self, kb: &str, diary: &str, subject_norm: &str,
                          relation: Option<&str>) -> Result<Vec<Fact>, StorageError>;
    /// Sets valid_until/close_reason/close_authority/closed_by and appends
    /// `fact.interval_closed`. Sets `superseded_by`/`superseded_at` whenever a
    /// SUCCESSOR FACT closed this interval — i.e. whenever `closed_by` names a
    /// fact — and leaves them NULL when nothing succeeded it (`stopped`,
    /// `retracted_source`, an explicit end date). The trigger is the presence
    /// of a successor, NOT `reason == "superseded"`: a caller correction closes
    /// with `close_reason = 'corrected'` and still has a successor, and keying
    /// the compat write off the reason would drop those rows out of
    /// `list_recent_conflicts:4776-4788` (which JOINs on `superseded_by`) and
    /// out of every unswept `superseded_by IS NULL` reader. That pair is the
    /// compatibility view every legacy reader still tests, and the event
    /// records what was written via `sets_superseded_by` so a rebuild
    /// reproduces it [R2].
    pub fn close_fact_interval(&self, fact_id: Uuid, at: DateTime<Utc>, reason: &str,
                               authority: &str, closed_by: Option<Uuid>, actor: &str)
        -> Result<bool, StorageError>;
    /// Kept, same signature, all seven existing call sites unchanged
    /// (`commands/facts.rs:480,:492,:630`;
    /// `fact_write.rs:1047,:1143,:1150,:1155`). Re-expressed over
    /// `close_fact_interval` so every close lands in the journal.
    pub fn supersede_fact_with(&self, old: Uuid, new: Uuid, resolution: &str,
                               valid_until: Option<DateTime<Utc>>) -> Result<bool, StorageError>;
}
```

**CW-4 is resolved without a signature change.** `supersede_fact_with` must not stamp
`authority = "heuristic"` unconditionally: `facts.rs:480` and `:492` call it with
`resolution = "caller"` for a caller's `facts resolve`, and a blanket `heuristic` makes the §5
safety audit report the exact opposite of the truth. (`facts.rs:630` is the *triage* sweep and
passes a `triage_*` tag — it is genuinely heuristic.) The authority is derived from the
`resolution` tag the caller already passes, and every value below is
`CloseAuthority::as_str()` verbatim, so the column vocabulary has exactly one source:

| `resolution` value | `close_authority` |
|---|---|
| `caller` | `caller` |
| `replayed_caller` | `replayed_caller` |
| `explicit_end` | `explicit_end` |
| `successor`, `slot_lww` | `successor_interval` |
| everything else (`exact_match`, `duplicate`, `semantic_llm`, `recency_*`, `triage_*`, `compile_*`, NULL) | `heuristic` |

New callers that know their authority call `close_fact_interval` directly. `close_reason` from
`supersede_fact_with` is always `superseded` (that is what supersession means), except
`resolution = "caller"` → `corrected`, matching the migration's mapping in §4 step 2 so a v9 row
and a v10 write of the same shape land identically. Because `supersede_fact_with` always names a
successor, it always writes `superseded_by`/`superseded_at` — including on the `corrected` path —
which is what keeps the v9 row and the v10 write byte-comparable and keeps caller closes visible
to `list_recent_conflicts`.

`close_fact_interval` does **not** consult `may_close`. The gate is fact-semantics' and runs in
the planner, before the applier: a storage method that silently refuses a write would make the
plan and the store disagree, which is the failure mode `plan_write` exists to prevent. Storage
records what it is told and records WHO told it, which is what makes the audit query possible.

### 2.9 The `superseded_by IS NULL` sweep — this lane's fourteen query sites [CD-2, IN-13]

```rust
/// A fact still asserts something only while BOTH are true: no successor took
/// its slot, and its own interval has not been closed. `close_reason='stopped'`
/// sets `valid_until` and leaves `superseded_by` NULL, so the legacy predicate
/// reads a stopped medication as current — the exact failure move 2 exists to
/// prevent, introduced by move 2. Mirrors `engram_core::interval::is_open_fact`.
const OPEN_INTERVAL_PREDICATE: &str = "(valid_until IS NULL AND superseded_by IS NULL)";
```

Sites in `crates/engram-storage/src/sqlite.rs`, all `superseded_by IS NULL` → the predicate above:

| line | function |
|---|---|
| 3987 | `get_active_facts` |
| 4016 | `get_active_facts_in_kb` |
| 4119 | `supersede_fact_with` guard (behaviour note below) |
| 4159 | `active_fact_embeddings` |
| 4212 | `distinct_fact_subjects` |
| 4563, 4579 | `list_facts_by_subject` |
| 4615, 4630 | `list_facts` |
| 4670, 4685 | `list_facts_scoped` |
| 4727, 4743 | `list_facts_by_subject_scoped` |
| 4796 | `count_facts` |

Fourteen, not fifteen: `rg -n 'superseded_by IS NULL' crates/engram-storage/src/sqlite.rs` returns
fifteen hits, and interfaces v2 §7's "fifteen sites" counts the `SCHEMA_V1` partial index at `:488`
among them. Fourteen are queries and are swept; `:488` is DDL and is not.

Site 488 (`idx_facts_active_lookup`, `SCHEMA_V1`) is left ALONE: a partial index predicate cannot
be altered in place, and `SCHEMA_V1` is re-executed on every open. v10 adds
`idx_facts_open_interval ON facts(kb, diary, subject_norm, predicate) WHERE valid_until IS NULL
AND superseded_by IS NULL`, which is what the rewritten queries plan against. Sites 4066
(`facts_matching_terms` ordering) and 4778 (`list_recent_conflicts`) test `IS NOT NULL` and are
history queries, not open-fact queries — unchanged.

Behaviour change at 4119, called out because it is the one that is not purely additive:
`supersede_fact_with` currently supersedes any fact without a successor, including one whose
interval was closed with `stopped`. After the sweep it returns `false` for an already-closed
interval. That is correct (an interval closes once) and read-surfaces' "already resolved" path
depends on it.

**Sequencing.** These fourteen sites land in wave 2, behind data that only wave 3 produces — until
fact-semantics ships `close_reason='stopped'`, `valid_until` is only ever set together with
`superseded_by`, so the new predicate is exactly equivalent to the old one and the change is
provably inert. read-surfaces' four sites (`facts.rs:123,156,157`, `recall.rs:310`) land at the
named wave-3 integration gate [R15/CD-2]; this lane's report names them and does not touch them.

### 2.9b The other three paths that still hard-delete facts [move 1, constraint 3]

Fixing `soft_delete_memory` and `delete_facts_for_memory` (§3.2) is necessary and not sufficient.
Three more live paths in this lane's own file delete fact rows, and every one of them cascades
`fact_conflict_queue` away through the FK at `sqlite.rs:1107-1108` — the exact mechanism that
erased 14.5k adjudication rows. After the soft-delete rewrite, `repair_integrity` in particular
becomes the *new* biggest destroyer of the ledger the rewrite just preserved: it deletes facts
whose memory is `deleted = 1`, which is precisely the set the rewrite now keeps as closed
intervals.

| site | today | this lane |
|---|---|---|
| `delete_kb:1320-1321` | `DELETE FROM facts WHERE kb = ?1` | unchanged: deleting a KB is meant to delete its data, and `kb.rs` is frozen. But the journal keeps that KB's events forever, so a later `admin rebuild --kb <name>` would resurrect every deleted fact. §2.12 therefore refuses `--kb K` when `K` is absent from `knowledge_bases` (`BadInput`, exit 3, naming the KB), and an unscoped `admin rebuild` replays only events whose `kb` still exists. No new `EventKind` is invented for this — the 15 kinds are pinned. |
| `repair_integrity:2879-2882` | `DELETE FROM facts WHERE source_memory_id IN (SELECT id FROM memories WHERE deleted = 1)` | **must change with the soft-delete fix, in the same commit**: close those intervals (`retracted_source`) instead of deleting them, and count them as `facts_from_deleted_memories_closed`. Shipping the soft-delete fix without this one means the next `engram repair` undoes it. `repair.rs` (the CLI command) is frozen; the storage method is this lane's, and the report field rename is CLP-9. |
| `hard_delete_memory:2085-2091` | `delete_facts_for_memory` then `DELETE FROM memories`, which CASCADEs facts via `facts.source_memory_id REFERENCES memories(id) ON DELETE CASCADE` (`SCHEMA_V1:483`) | kept as a genuine hard delete — it is ingest rollback for a memory that never became visible — but it now runs BEFORE any event for that memory could matter, and it is the one caller for which the rewritten `close_facts_for_memory` is wrong. It therefore calls a new `purge_facts_for_memory` (the old delete body, renamed and doc-commented "ingest rollback only"), so the rename cannot silently turn rollback into a pile of closed intervals for a memory that no longer exists. |

The `source_memory_id … ON DELETE CASCADE` FK itself is NOT dropped in this lane: dropping a FK in
SQLite is a table rebuild of the largest table in the store, inside the same migration that already
rewrites every fact row, and no path outside `hard_delete_memory` hard-deletes a memory row. It is
recorded as OQ-12 instead.

### 2.10 `StorageError` [R11]

`crates/engram-storage/src/error.rs` gains two variants and two predicates so contract-codegen's
mapping table is a total function over things it can actually observe:

```rust
    #[error("database is busy: {0}")]
    Busy(String),
    #[error("malformed row: {0}")]
    MalformedRow(String),

impl StorageError {
    /// SQLITE_BUSY / SQLITE_LOCKED under any wrapping, so the CLI maps to
    /// SqliteBusy → exit 1 rather than a generic transient.
    pub fn is_busy(&self) -> bool;
    pub fn is_not_found(&self) -> bool;
}
```

No existing code matches `StorageError` exhaustively (two constructor sites only:
`fact_write.rs:871`, `edit.rs:186`), so the variants are additive in practice as well as in
principle. `with_tx` and `begin_immediate` translate a rusqlite busy failure into
`StorageError::Busy` so a caller can retry without string-matching.

### 2.11 Conflict resolution as a projection of a decision [move 4's race, at the storage layer]

The queue row's `status` is a projection of `decision.recorded`. Making that structural rather
than a caller discipline also closes the concurrent-`facts resolve` race the brief names
(`facts.rs:451-506` is non-transactional and ignores the claim boolean, so two racing resolves can
supersede both facts and both report success):

```rust
impl SqliteStore {
    /// Claim the row and record the decision in ONE transaction. The UPDATE is
    /// conditional (`WHERE id=? AND status='open'`); zero rows updated means
    /// somebody else won the claim, and the method returns Ok(None) so the
    /// caller can report `conflict_already_resolved` (exit 3) instead of
    /// reporting a success it did not perform.
    #[allow(clippy::too_many_arguments)]
    pub fn resolve_conflict_with_decision(&self, conflict_id: Uuid, kb: &str, diary: &str,
        key_encoded: &str, verdict: &str, authority: &str, actor: &str, note: Option<&str>,
        key_version: &str) -> Result<Option<Uuid>, StorageError>;
}
```

`resolve_fact_conflict:4443` is kept unchanged for compatibility, with a doc comment saying it
records no decision and that new callers use the method above. `resolve_orphan_fact_conflicts:4378`
appends one `decision.recorded { verdict: "void", authority: "machine",
note: "auto: referenced facts no longer exist" }` per row it closes, using the row's
`decision_key` when present and the synthetic key `orphan:<conflict_id>` when it is NULL — a key
shaped so it can never collide with a real `SemanticKey` encoding and therefore can never replay.

### 2.12 `engram admin rebuild` [invariant 3, IN-14, CD-4]

New command, this lane's (interfaces v2 §7). Surface:

```
engram admin rebuild [--kb <name>] [--dry-run] [--confirm] [--json]
```

**What it rebuilds, and what it honestly does not.** Invariant 3 says "drops and replays
projections". The journal is the source of truth for the *adjudication and fact layer*; it is not
a backup of everything in the file:

| table | rebuilt? | why |
|---|---|---|
| `facts` | yes, except `embedding` / `embed_model` | embeddings are a recomputable cache; `reindex` owns them, and a 768-float blob has no business in a JSON event payload |
| `fact_conflict_queue` | yes | from `conflict.opened` + `decision.recorded` |
| `fact_decisions` | yes | from `decision.recorded` |
| `derivations`, `fact_extraction_log` | yes | from `derivation.ran` |
| `memories`, `chunks` | **no** | `access_count`, `last_accessed`, `stability` and chunk embeddings are deliberately not journaled; `memory.*` events are provenance for them, not their source |
| `claims`, `entities`, `relations`, `wiki_pages`, `source_spans` | **no** | compiler caches; `engram compile` regenerates them |

The command PRINTS this table (`skipped: [...]` with reasons) on every run. A rebuild that
implies it restored things it did not is the class of dishonest health check 0.3.1 spent a
release removing.

Algorithm, inside one `with_tx`:

0. `--kb K` where `K` is absent from `knowledge_bases` is `BadInput`, exit 3, naming the KB: the
   journal still holds a deleted KB's events (`delete_kb` deletes rows, not history), and replaying
   them would resurrect data the user deleted (§2.9b). An unscoped rebuild replays only events
   whose `kb` still exists, and reports the rest as `events_skipped_deleted_kb`.
1. `DELETE FROM fact_conflict_queue / fact_decisions / derivations / fact_extraction_log`, then
   `DELETE FROM facts`, scoped by `kb` when `--kb` is given. **The queue goes first on purpose**:
   deleting facts first cascades queue rows away through `sqlite.rs:1107-1108`, and under `--kb`
   that cascade reaches rows in OTHER KBs whose pair happens to straddle the boundary — a scoped
   rebuild that quietly deletes another KB's questions is the same class of defect as the one this
   lane exists to fix. Facts' embeddings are read into a `HashMap<Uuid, (Vec<u8>, String)>` first
   and re-applied after replay, so a rebuild does not silently cost the store its dense fact search.
2. Stream `events_slice(0, kb, None)` in `seq` order through `rebuild::apply(event)`.
3. `--dry-run` replays into `ATTACH ':memory:' AS shadow` copies of the five tables and diffs,
   printing counts per table and the first ten differing ids. It writes nothing and needs no
   `--confirm`.
4. Without `--dry-run`, `--confirm` is required and its absence is `BadInput` (exit 3) with the
   exact command to re-run. The brief's destructive-command precedent (`kb delete`, `repair`) is
   `--confirm`, not `--yes`.

Replay dispatch (`crates/engram-storage/src/rebuild.rs`), by `EventKind`:

| event | effect |
|---|---|
| `fact.observed` | INSERT the `facts` row; `diary`/`kb` from the event columns, everything else from the payload |
| `fact.interval_closed` | UPDATE `valid_until`, `close_reason`, `close_authority`, `closed_by`; also `superseded_by`/`superseded_at` when `sets_superseded_by` is true [R2] |
| `fact.confirmed` | `confirmations = confirmations + 1` |
| `derivation.ran` | UPSERT `derivations` + `fact_extraction_log` |
| `conflict.opened` | INSERT the queue row, `status='open'` |
| `decision.recorded` | INSERT `fact_decisions`; when `conflict_id` is present, set that queue row to `resolved` (or `superseded_question` when the payload carries `collapsed_into`) |
| `decision.replayed` | no projection effect — it is an audit record of an applied replay |
| `memory.*`, `compile.ran`, `job.*` | ignored by rebuild (see the table above) |
| unknown kind string | `StorageError::MalformedRow` — a skipped event is a projection that silently diverges from its journal |

Three replay rules that are cheap to state and expensive to rediscover:

- **Replay writes raw SQL, never the journaling methods.** `rebuild::apply` must not call
  `insert_fact` / `close_fact_interval` / `record_decision`: those append events, and a rebuild
  that appends is a rebuild that grows its own input. `apply` takes `&EventRow` and issues one
  INSERT or UPDATE per event, against the same column list the journaling method uses.
- **`facts.kb` comes from the event's `kb` column, not from a subquery.** `insert_fact:3937`
  derives `kb` with `COALESCE((SELECT kb FROM memories WHERE id = ?2), 'default')` because `Fact`
  has no `kb` field — so the live path must read that value back and put it on the event's `kb`
  column, and replay must use it. Re-running the subquery at replay time silently re-homes every
  fact whose source memory was later moved between KBs, and the byte-identical property fails.
- **`conflict.opened` replay runs with `PRAGMA defer_foreign_keys = ON` for the transaction.** The
  queue's two FKs to `facts` are `ON DELETE CASCADE` (`sqlite.rs:1107-1108`); an orphaned queue row
  — legitimately preserved by the migration with a NULL `decision_key` (§4.5) — has no facts to
  point at, and a plain INSERT would abort the whole rebuild. Deferring checks to COMMIT is wrong
  too (it would still abort), so replay inserts orphan queue rows only when both facts exist and
  otherwise counts them into `RebuildReport.conflicts_skipped_orphan`, printed with the skipped
  table. This is the one place a rebuild is knowingly not byte-identical, and it says so.

**Payload additions this lane makes** (additive; the contract says readers ignore unknown fields,
and each is reported to the lead for interfaces v2.1 — see §7):

- `fact.observed` gains `fact_type` and `diary`. Without `fact_type` a rebuilt fact comes back
  `assertion` and the byte-identical property fails on the first `profile_slot` row in any store.
- `conflict.opened` gains `suggested` (the queue column read by every conflicts listing) and
  `note`.
- `decision.recorded` gains optional `collapsed_into: <uuid>`, set only by the migration's
  duplicate-key collapse, so `status='superseded_question'` is rebuildable rather than inferred
  from note text.

### 2.13 Export v3 — journal slice, replay, and v2-genesis synthesis [invariant 4, CD-9]

`crates/engram-storage/src/export_v3.rs` (NEW). This lane owns the format, the writer and the
replayer as library functions; write-surfaces wires `commands/export.rs` / `import.rs` to them in
wave 3 (`export.rs` today emits `"version": 2` at `:28`).

```rust
pub const EXPORT_VERSION: i64 = 3;

#[derive(Serialize, Deserialize)]
pub struct ExportV3 {
    pub version: i64,               // 3
    pub schema_version: i32,        // store schema at export time (10)
    pub kb: Option<String>,
    pub exported_at: String,
    pub counts: ExportCounts,
    /// The journal slice, in `seq` order. Lossless: this is the source of truth.
    pub events: Vec<ExportEvent>,
    /// v2 sections, retained verbatim so a v3 file also restores the things the
    /// journal deliberately does not carry: chunk embeddings, access telemetry,
    /// documents, KBs.
    pub kbs: Vec<KbExportRow>,
    pub documents: Vec<DocumentExportRow>,
    pub memories: Vec<MemoryExportRow>,
    pub chunks: Vec<ChunkExportRow>,
    pub facts: Vec<FactExportRow>,
}
#[derive(Serialize, Deserialize)]
pub struct ExportEvent { pub seq: i64, pub id: String, pub ts: String, pub actor: String,
                         pub kind: String, pub subject_id: Option<String>,
                         pub kb: Option<String>, pub diary: Option<String>,
                         pub payload: serde_json::Value }

impl SqliteStore {
    pub fn export_v3(&self, kb: Option<&str>) -> Result<ExportV3, StorageError>;
    /// Replay an import: v2 rows first (memories/chunks/documents — the things
    /// the journal does not carry), then the event slice through the same
    /// `rebuild::apply` dispatcher, so import and `admin rebuild` cannot drift.
    pub fn import_v3(&self, doc: &ExportV3) -> Result<ImportV3Report, StorageError>;
    /// v2 files stay importable, and the missing journal is SYNTHESIZED from
    /// the rows: one `memory.written` per memory, one `fact.observed` per fact,
    /// one `fact.interval_closed` per closed fact — the same generator the
    /// v9→v10 migration uses (§4 step 3). A v2 import that produced no journal
    /// would leave an un-rebuildable store, and invariant 3 would fail on it.
    pub fn import_v2_with_genesis(&self, kbs: &[KbExportRow], documents: &[DocumentExportRow],
        memories: &[MemoryExportRow], chunks: &[ChunkExportRow], facts: &[FactExportRow],
        actor: &str) -> Result<ImportV3Report, StorageError>;
}
pub struct ImportV3Report { pub kbs: usize, pub documents: usize, pub memories: usize,
                            pub chunks: usize, pub facts: usize, pub events: usize,
                            pub events_skipped_duplicate: usize, pub genesis_synthesized: usize }
```

Rules that are not obvious and must not be re-derived:

- **Event ids are preserved on import**; `events.id` is `UNIQUE`, so re-importing the same file is
  idempotent (`INSERT OR IGNORE`, counted into `events_skipped_duplicate`). `seq` is NOT
  preserved — it is the local append order, and two merged slices cannot share one sequence.
  Replay order is the file's `seq` order, which is why `ExportEvent` carries it.
- Import runs inside one `with_tx`, and the v2 sections land BEFORE the events, so a
  `fact.observed` replay that touches a memory finds it.
- **Facts appear in both sections, and the precedence is pinned**: the `facts` section is imported
  first with `INSERT OR IGNORE` — it exists solely to carry the columns the journal deliberately
  does not (`embedding`, `embed_model`, `access`-style telemetry) — and the event replay is then
  authoritative for everything else, `fact.observed` being `INSERT OR IGNORE` and
  `fact.interval_closed` an `UPDATE`. Without that ordering rule the two sections race:
  replay-then-rows loses the interval closes, and rows-then-replay without `OR IGNORE` aborts on
  `facts.id` UNIQUE. `ImportV3Report.facts` counts rows actually inserted by the first pass, so a
  v3 file whose facts section is empty (a legal, journal-only slice) reports `facts: 0` and still
  restores every fact through replay, minus its embeddings — which `reindex` regenerates.
- A v3 file whose `schema_version` exceeds the binary's is refused with
  `StorageError::Migration`, matching `migrate()`'s existing refusal at `sqlite.rs:878-883`.
- `export_v3(Some(kb))` emits `events WHERE kb = ?` plus events with `kb IS NULL` whose
  `subject_id` is a memory/fact in that KB; the latter is a single `IN (SELECT …)` and exists so a
  per-KB export is not silently missing its own genesis events.

---

## 3. File-by-file change plan

### 3.1 New files

| File | Size | Contents |
|---|---|---|
| `crates/engram-core/src/keys.rs` | ~120 | §2.1. `SEMANTIC_KEY_NAMESPACE`, `SEMANTIC_KEY_VERSION`, `CONTENT_HASH_NAMESPACE`, `object_pair_hash`, `semantic_key_encode/decode`, `content_hash`, unit tests (order-independence, round-trip, separator-in-value). |
| `crates/engram-storage/src/journal.rs` | ~520 | §2.2. DDL const, `EventDraft`/`EventRow`, `append_event`, `events_for_subject`, `events_slice`, `event_by_id`, `max_event_seq`, `canonical_json`, `payload::*` builders, `EXTRACTOR_*` consts, unit tests. |
| `crates/engram-storage/src/rebuild.rs` | ~380 | §2.12. `apply(&SqliteStore, &EventRow)`, `rebuild_projections(&SqliteStore, Option<&str>, DryRun)`, `RebuildReport`, the projection-scope table as a doc comment, unit tests. |
| `crates/engram-storage/src/migrate_v10.rs` | ~700 | §4. `impl SqliteStore { pub(crate) fn migrate_v10(&self) -> Result<MigrationV10Report, StorageError> }` plus its private step functions and genesis-event generator (shared with `export_v3::import_v2_with_genesis`). |
| `crates/engram-storage/src/backup.rs` | ~90 | §4 step 0. `pub(crate) fn vacuum_into_backup(conn: &Connection, data_dir: &Path) -> Result<PathBuf, StorageError>` + filename builder + unit tests. |
| `crates/engram-storage/src/export_v3.rs` | ~450 | §2.13. `ExportV3`, `ExportEvent`, `ImportV3Report`, `export_v3`, `import_v3`, `import_v2_with_genesis`. |
| `crates/engram-cli/src/commands/admin.rs` | ~180 | §2.12. `pub fn rebuild(ctx: &AppContext, kb: Option<String>, dry_run: bool, confirm: bool) -> Result<(), CliError>`; envelope via `print_success`/`Metadata` like `commands/kb.rs`; the skipped-tables table in both JSON and text. |

`crates/engram-core/src/relations.rs` and `interval.rs` are already in the tree (§2.1). This lane
adds only the conformance tests 1-10 to their existing `#[cfg(test)]` modules and reports any
drift from interfaces v2 §3.1-§3.2 rather than fixing it in place — the content is fact-semantics'.

### 3.2 `crates/engram-storage/src/sqlite.rs` — edits, with anchors

| Anchor | Change |
|---|---|
| `:26` `conn: Connection` | → `pub(crate) conn: Connection` so `journal.rs`, `rebuild.rs`, `migrate_v10.rs`, `backup.rs`, `export_v3.rs` can prepare statements. |
| `:308-334` `FactExportRow` | +`closed_by`, `close_reason`, `close_authority`, all `#[serde(default)]`. `export_facts:4912` and `import_fact_row:5050` gain the three columns. |
| `:346-360` `FactConflictRow` | +`decision_key: Option<String>`, `member_key: Option<String>`, `registry_version: Option<String>`; `parse_conflict_row:362` reads indices 13-15 and every `SELECT` that feeds it (`:4263`, `:4275`, `:4322`, `:4484`, `:4763`) gains the three columns. |
| `:383-492` `SCHEMA_V1` | untouched. Fresh stores get v10 objects through the migration chain, exactly as v6-v9 do today. Site `:488`'s partial index stays (see §2.9). |
| `:792` `CURRENT_VERSION: i32 = 9` | → `10`. |
| `:795-814` `open()` | before `store.migrate()`: read the stored version with `try_schema_version()`; when it is `Some(v)` with `v < 10`, call `backup::vacuum_into_backup` — OUTSIDE any transaction, because `VACUUM INTO` cannot run inside one. Fresh stores (`None`) and up-to-date stores are not backed up. |
| `:816-825` `open_in_memory()` | unchanged; never backs up (CD-7) — the check keys off `self.path == ":memory:"` as well as the version, so a future in-memory caller cannot acquire a backup file by accident. |
| `:831-849` `with_tx` | + busy translation (§2.10). Nesting behaviour (joins an open tx) is unchanged and is what makes invariant 1 composable. |
| `:870-936` `migrate()` | + `if current < 10 { s.migrate_v10()?; }` after the `current < 9` arm. |
| `:1855-1870` `soft_delete_memory` | STOP hard-deleting facts (`DELETE FROM facts WHERE source_memory_id`, `:1862-1865`). Replace with `close_fact_interval(reason="retracted_source", authority="explicit_end", closed_by=None)` per open fact of that memory, + `memory.retracted`. This is the brief's `sqlite.rs:1855-1869` defect. `delete_derived_for_memory_chunks:1878` (claims/spans) is unchanged — those are caches. |
| `:1897-1919` `update_memory` | + `memory.revised` (content changed, with `content_hash`/`prev_content_hash`) or `memory.meta_changed` (importance only), inside `with_tx`. |
| `:1925-1935` `delete_facts_for_memory` | **deprecated, body replaced.** No longer deletes: closes each of the memory's open fact intervals with `retracted_source` and deletes the `fact_extraction_log` marker only. Renamed `close_facts_for_memory`, with `delete_facts_for_memory` kept as a plain alias and a doc comment saying what it now does — **no `#[deprecated]` attribute**, because the deprecation warning would fail `clippy -D warnings` in the lane that still calls it (`edit.rs:190`). This is the brief's headline defect (edit hard-deleting facts and cascading the adjudication ledger away). |
| `:2085-2091` `hard_delete_memory` | calls the renamed-and-preserved `purge_facts_for_memory` (the old delete body), NOT `close_facts_for_memory` — ingest rollback must still leave nothing behind (§2.9b). |
| `:2879-2882` `repair_integrity` | STOP deleting facts whose memory is `deleted = 1`; close their intervals with `retracted_source` instead, in the same commit as the `soft_delete_memory` change (§2.9b). `RepairReport` (`sqlite.rs:67`) keeps `facts_from_deleted_memories_removed` — it is a published JSON key at `repair.rs:18`, and constraint 3 forbids renaming it — now always `0`, and gains `facts_from_deleted_memories_closed` beside it. CLP-9 adds the second key to `repair.rs`'s output; the first stays so no consumer breaks. |
| `:1971-1995` `record_fact_extraction` | kept; `record_derivation` (§2.4) calls it and adds the content-keyed row + event. |
| `:3937-3969` `insert_fact` | + the three new columns; + `fact.observed`. |
| `:3974-4025` `get_active_facts`, `get_active_facts_in_kb` | sweep sites `:3987`, `:4016` (§2.9); both SELECT lists gain `closed_by, close_reason, close_authority`. |
| `:4080-4093` `get_fact` | SELECT list gains the three columns. |
| `:4097-4129` `supersede_fact`, `supersede_fact_with` | re-expressed over `close_fact_interval`; authority derived from `resolution` (§2.8 table); guard at `:4119` gains the interval predicate. |
| `:4175-4181` `increment_fact_confirmations` | + `fact.confirmed`. |
| `:4229-4258` `insert_fact_conflict` | new signature + `InsertOutcome` + `conflict.opened` (§2.6). Exactly one caller today (`fact_write.rs:1214`), which fact-semantics rewrites in wave 3; the wave-2 build stays green via the `cross_lane_patches` entry in §7. |
| `:4378-4396` `resolve_orphan_fact_conflicts` | + one void `decision.recorded` per closed row (§2.11). |
| `:4443-4462` `resolve_fact_conflict` | unchanged body; doc comment gains "records no decision; new callers use `resolve_conflict_with_decision`". |
| `:4542-4760` `list_facts_by_subject`, `list_facts`, `list_facts_scoped`, `list_facts_by_subject_scoped` | sweep sites (§2.9); SELECT lists gain the three columns. |
| `:4794-4803` `count_facts` | sweep site `:4796`. |
| `:4805-4950` `export_*` | `export_facts` gains three columns; the rest untouched (export v3 reuses them). |
| `:5050-5082` `import_fact_row` | gains three columns, `#[serde(default)]`-compatible with v2 files. |
| `:5370-5440` `parse_fact_row` / `parse_fact_from_offset` | read the three new columns at fixed offsets. The **caller-side offsets shift and every one is pinned here**, because a wrong offset mis-parses silently: a fact row goes 18 → 21 columns and a conflict row 13 → 16, so `list_conflicts_with_facts:4366-4367` becomes `parse_fact_from_offset(r, 16)` / `(r, 37)` (was 13 / 31) and `list_recent_conflicts:4787-4788` becomes `(r, 0)` / `(r, 21)` (was 0 / 18). Every `SELECT` feeding these must list the fact columns in `parse_fact_row` order with the three new ones appended LAST. Unit test: a full-row round-trip through both joined shapes, asserting the second fact's id, not just its arity. |
| new methods | `facts_for_relation`, `open_intervals`, `close_fact_interval`, `resolve_conflict_with_decision`, `record_decision`, `lookup_decision`, `decisions_for_key`, `record_derivation`, `derivation_for`, `derivations_for_memory`, `record_replay`, `resolve_handle`, `admin`-facing `rebuild_projections` wrapper. |

`crates/engram-storage/src/lib.rs`: `pub mod backup; pub mod export_v3; pub mod journal; pub mod
migrate_v10; pub mod rebuild;` and re-exports `DecisionRow`, `DerivationRow`, `EventDraft`,
`EventRow`, `ExportV3`, `ImportV3Report`, `InsertOutcome`, `RebuildReport`, `Resolved`.

`crates/engram-storage/src/error.rs`: §2.10.

### 3.3 `crates/engram-cli/src/commands/admin.rs` (NEW, this lane's only engram-cli file)

Mirrors `commands/kb.rs`'s shape: takes `&AppContext`, returns `Result<(), CliError>`, prints
through `crate::output::{print_success, Metadata}`. It contains no SQL — the whole algorithm is
`ctx.store.rebuild_projections(kb.as_deref(), dry_run)` returning `RebuildReport`, which the
command renders. Keeping the logic in `engram-storage` is what lets the byte-identical property
test run without spawning a binary.

Text output names the skipped tables and why; JSON output is
`{ "rebuilt": { "facts": n, … }, "skipped": [ { "table": "chunks", "reason": "…" } ],
"dry_run": bool, "kb": … }`.

### 3.4 Files this lane does NOT touch, and who patches them

`cli.rs` (the `Admin` subcommand definition) is contract-codegen's; `lib.rs` (the dispatch arm)
is daemon-jobs'; `commands/mod.rs` (`pub mod admin;`) is the lead's. All three are one-liners and
are listed verbatim in §6.

---

## 4. Migration and compatibility

`interfaces.md` §2.1 is the **binding contract** [R5]; `spec-fact-semantics.md` §4.1 Phase 1 is
its annex and supplies the mapping tables. Where the annex's prose and interfaces v2 disagree —
the backup filename is the live case — interfaces v2 wins: the file is **timestamped**
(`[R5a]` overrules the annex's line 881, which followed interfaces v1).

`CURRENT_VERSION` 9 → 10. `migrate()` gains `if current < 10 { s.migrate_v10()?; }`.

### 4.0 Step 0 — the backup, outside the transaction [R5a, CD-7, CC-2]

There is **no** backup mechanism anywhere in the tree today; `migrate_v8:1054` writes no file and
the brief's "repo precedent" does not exist. This is new code.

```rust
// crates/engram-storage/src/backup.rs
/// `VACUUM INTO '<data_dir>/db.sqlite.pre-v10-backup.<YYYYMMDDHHMMSS>'` (UTC).
/// VACUUM INTO, not a file copy: the store runs in WAL mode, so copying
/// db.sqlite without its -wal and -shm siblings yields a torn database, and
/// copying all three races anything still holding them.
pub(crate) fn vacuum_into_backup(conn: &Connection, db_path: &Path)
    -> Result<PathBuf, StorageError>;
```

- Called from `open()` **before** `migrate()`, and only when `try_schema_version()` returns
  `Some(v)` with `v < 10`. A fresh store (`None`) has nothing to back up; an up-to-date store is
  not touched, so this does not run on every CLI invocation.
- **Never** for `open_in_memory()`. The guard is two-part — `self.path != Path::new(":memory:")`
  as well as the version test — so a future in-memory caller cannot acquire a backup file by
  accident (CD-7).
- Filename collision (two migrations inside one second, or a restored store re-migrated):
  suffix `-1`, `-2`, … up to `-99`, then `StorageError::Migration`. `VACUUM INTO` refuses to
  overwrite, and silently reusing a name would destroy the only rollback artefact.
- A failed backup **aborts the migration** with `StorageError::Migration` naming the path and the
  OS error. Migrating without a backup is not a degraded mode; it is the failure this step exists
  to prevent. The message tells the user the one thing that fixes it (free space / permissions on
  the store directory) and that the store is untouched.
- `VACUUM INTO` transiently needs disk equal to the store size. Documented in the error text, and
  in the CHANGELOG, because a 2 GB store on a full disk fails an upgrade that used to succeed.

### 4.1 Step 1 — DDL, in this order [IN-11]

Verbatim from IN-11 / `spec-fact-semantics.md` §2.6, plus this lane's two tables:

```sql
ALTER TABLE facts ADD COLUMN closed_by       TEXT;
ALTER TABLE facts ADD COLUMN close_reason    TEXT;
ALTER TABLE facts ADD COLUMN close_authority TEXT;
CREATE INDEX IF NOT EXISTS idx_facts_open_interval
    ON facts(kb, diary, subject_norm, predicate)
    WHERE valid_until IS NULL AND superseded_by IS NULL;
CREATE INDEX IF NOT EXISTS idx_facts_relation ON facts(kb, diary, subject_norm, predicate);
ALTER TABLE fact_conflict_queue ADD COLUMN decision_key     TEXT;
ALTER TABLE fact_conflict_queue ADD COLUMN member_key       TEXT;
ALTER TABLE fact_conflict_queue ADD COLUMN registry_version TEXT;
-- events (§2.2), fact_decisions (§2.5), derivations (§2.4), migration_log
-- CREATE UNIQUE INDEX idx_fcq_open_key is NOT created here — see step 5.3.
CREATE TABLE IF NOT EXISTS migration_log (
    version INTEGER PRIMARY KEY, ran_at TEXT NOT NULL, summary_json TEXT NOT NULL);
```

All `ALTER`s go through the existing `add_column_if_missing:1194`, which is already idempotent.

### 4.2 Step 2 — interval backfill, with the clamp in SQL

```sql
UPDATE facts SET valid_from = COALESCE(valid_from, created_at) WHERE valid_from IS NULL;

UPDATE facts
   SET valid_until = MAX(valid_from, COALESCE(valid_until, superseded_at, created_at)),
       closed_by   = COALESCE(closed_by, superseded_by),
       close_reason = COALESCE(close_reason,
           CASE WHEN resolution = 'caller' THEN 'corrected' ELSE 'superseded' END),
       close_authority = COALESCE(close_authority,
           CASE WHEN resolution = 'caller' THEN 'caller' ELSE 'heuristic' END)
 WHERE superseded_by IS NOT NULL;
```

The `MAX` is `engram_core::interval::close_at`'s clamp expressed in SQL and is **not optional**: a
fact whose stated `valid_from` post-dates its write time (a future-dated `lives_in`, an import)
would otherwise migrate to `valid_until < valid_from`, a negative-length interval that `overlaps`
and every downstream consumer read as nonsense. SQLite's two-argument `MAX` is the scalar form and
compares RFC3339 strings, which sort chronologically here for the reason `MEMORY_CONSTRAINS_DERIVED`
(`sqlite.rs:752-781`) already documents.

Mapping tables, pinned:

| `facts.resolution` | `close_reason` | `close_authority` |
|---|---|---|
| `caller` | `corrected` | `caller` |
| `duplicate` | `superseded` | `heuristic` |
| `slot_lww`, `exact_match`, `semantic_llm`, `recency_*`, `triage_*`, `compile_*`, NULL | `superseded` | `heuristic` |

A third case the `WHERE superseded_by IS NOT NULL` clause misses, and it must not be left NULL: a
fact with `valid_until` set and `superseded_by` NULL. No live v9 write path produces one
(`supersede_fact_with:4118` always writes both), but `import_fact_row:5050` restores whatever a v2
export file carries, so a store built from an import can hold them. They are closed intervals with
no successor, and step 3 would otherwise synthesize a `fact.interval_closed` whose `reason` and
`authority` are NULL — an unreplayable event. One more statement, after the one above:

```sql
UPDATE facts SET close_reason = 'stopped', close_authority = 'explicit_end'
 WHERE valid_until IS NOT NULL AND superseded_by IS NULL AND close_reason IS NULL;
```

Rows with `superseded_by IS NULL` keep `valid_until` NULL — they are the open intervals, and they
are what `engram state` will read.

### 4.3 Step 3 — genesis events

Generated by `migrate_v10::genesis`, the SAME generator `export_v3::import_v2_with_genesis` uses,
so a v2 import and a v9 migration produce identical journals for identical rows.

Order (it is the replay order, and `admin rebuild` depends on it):

1. `memory.written` per row of `memories`, `ORDER BY created_at, id`.
2. `derivation.ran` per row of `fact_extraction_log`, joined to its memory for the content hash;
   also inserts the `derivations` projection row (§2.4).
3. `fact.observed` per row of `facts`, `ORDER BY created_at, id`.
4. `fact.interval_closed` per fact with `valid_until IS NOT NULL`, carrying the step-2 mapped
   `reason`, `authority`, `closed_by`, and `sets_superseded_by = (superseded_by IS NOT NULL)` [R2].
5. Queue rows — step 4 and step 5 below.

Two properties make the whole migration idempotent almost for free:

- **Genesis event ids are deterministic**:
  `id = Uuid::new_v5(GENESIS_NAMESPACE, "<kind>\x1f<subject_id>\x1f<discriminator>")`, where the
  discriminator is `""` for the one-per-row events and the conflict/decision id for queue events.
  Every genesis insert is `INSERT OR IGNORE` on the `UNIQUE(id)`, so a re-run appends zero events
  — which is what the annex's `WHERE NOT EXISTS (SELECT 1 FROM events …)` guard asks for, done
  once instead of per-step.
- **Genesis timestamps are the source row's own**: `ts = created_at` (or `superseded_at` for
  `fact.interval_closed`), not `now()`. Stamping 15k events with one timestamp would erase the
  ordering information the journal exists to preserve. `actor = "migration:v10"` and the payload
  carries `"genesis": true`, so a reader can always tell a synthesized event from an observed one.

### 4.4 Step 4 — resolved queue rows → decisions (~14.5k)

Join each `status != 'open'` row to both facts. Infer the verdict:

| condition | verdict |
|---|---|
| `new.superseded_by == old.id` | `reject_new` |
| `old.superseded_by == new.id` | `close_old` |
| neither | `keep_both` |

Authority: `resolved_by = 'caller'` → `caller`; `'triage'`, `'compile'`, NULL, anything else →
`machine`. `decision_key = keys::semantic_key_encode(new.subject_norm,
canonical_relation(new.predicate), object_pair_hash(new.object_norm, old.object_norm))`,
`key_version = "semantic_key@1"`.

Emit `conflict.opened` then `decision.recorded`, and insert the `fact_decisions` projection row.
Rows whose facts no longer exist (a `delete_facts_for_memory` cascade took them) are **skipped**
and counted as `queue_rows_skipped_orphan` — there is nothing left to key them by, and inventing a
key would create a decision that replays onto an unrelated pair.

Also backfill `decision_key`, `member_key`, `registry_version` on the resolved rows themselves, so
`facts conflicts --all` can show what a historical question was about.

### 4.5 Step 5 — open queue rows, the collapse, then the unique index

**5.1** Emit `conflict.opened` and backfill `decision_key`, `member_key`
(`member_identity(relation, new.object_norm)`), `registry_version` on every `status='open'` row.
Rows whose facts are gone keep `decision_key = NULL`; the index's `AND decision_key IS NOT NULL`
clause is exactly what lets them stay open and visible.

**5.2 — the duplicate-key collapse, which is the step whose absence aborts the migration.** A
partial UNIQUE index does not merge duplicates, it rejects them, and duplicate semantic keys among
the open rows are not a corner case: they ARE the storm (the same pair re-queued on every
re-derivation).

```sql
SELECT kb, decision_key, id, created_at FROM fact_conflict_queue
 WHERE status='open' AND decision_key IS NOT NULL
 ORDER BY kb, decision_key, created_at ASC, id ASC;
```

Per `(kb, decision_key)` group with more than one row: the **survivor** is the lowest `created_at`,
ties broken by `id` — the question as first asked, whose id may already be in an agent's notes or
in a saved `resolve_command`. Every other row gets
`status='superseded_question'`, `resolved_at=now`, `resolved_by='migration'`, plus a
`decision.recorded { verdict: "void", authority: "machine", conflict_id: <loser>,
collapsed_into: <survivor> }`. `superseded_question` is never `resolved`, because nobody answered
it, and it is invisible to every `status='open'` listing. The count lands in the summary as
`open_conflicts_collapsed`.

**5.3** Only now:
`CREATE UNIQUE INDEX IF NOT EXISTS idx_fcq_open_key ON fact_conflict_queue(kb, decision_key) WHERE status='open' AND decision_key IS NOT NULL;`
Creating it before 5.2 is a guaranteed abort on any store that has ever queued the same pair twice
— which is every store with a backlog.

### 4.6 Step 6 — summary, idempotency, and cost

```rust
pub struct MigrationV10Report {
    pub backup_path: Option<PathBuf>, pub facts_backfilled: usize, pub intervals_closed: usize,
    pub genesis_events: usize, pub decisions_recorded: usize, pub derivations_backfilled: usize,
    pub open_conflicts_keyed: usize, pub open_conflicts_collapsed: usize,
    pub queue_rows_skipped_orphan: usize, pub elapsed_ms: u128,
}
```

Written to `migration_log(10, ran_at, summary_json)` and logged with `tracing::info!`. It is not
printed to stdout: `migrate()` runs inside `open()`, under every command including `--json` ones,
and a migration that prints to stdout corrupts the JSON envelope of whatever the agent actually
asked for. `engram doctor` reads `migration_log` in a later wave.

Idempotency, restated as the implementation rule: every genesis event is `INSERT OR IGNORE` on a
deterministic id; every projection insert is `INSERT OR IGNORE` on its primary key; every backfill
`UPDATE` is `COALESCE`-guarded or filtered to rows that still need it; step 5.2 is a no-op on a
re-run because the collapsed rows are no longer `status='open'`; step 5.3 is `IF NOT EXISTS`.
Running `migrate_v10` twice produces zero additional events and zero additional rows — asserted by
a test that runs it twice against one temp store and compares full-table digests.

Cost: one transaction, ~1 event per fact plus ~2 per queue row. On the measured backlog (951 open,
~14.5k resolved) that is ~30k events. Use prepared statements hoisted out of the loops; the budget
is **under 30 s** for a 100k-row store on a laptop. If it exceeds that, the fix is statement
hoisting and `PRAGMA synchronous=OFF` for the duration of the migration transaction only — never
splitting the transaction, because a half-migrated store has no honest version number.

### 4.7 Compatibility

- **v10 store, v9 binary**: refused by `migrate()`'s existing forward-version check
  (`:878-883`, `:892-897`) with the message that already tells the user to upgrade rather than
  downgrade. The pre-v10 backup is the downgrade path, and the CHANGELOG names it.
- **CLI surface**: unchanged. One new command (`engram admin rebuild`), additive.
- **JSON**: `Fact` gains three fields, `FactExportRow` gains three, `FactConflictRow` gains three.
  All `#[serde(default)]`, all `null` on a store that has not exercised them. No field is renamed,
  retyped or removed.
- **Export v2 files** stay importable forever (§2.13), and now synthesize a journal so an imported
  store is rebuildable.
- **`bench personal --json`** must be unchanged: it exercises recall, not adjudication. The lane's
  gate runs it before and after the migration on a seeded temp store and diffs the JSON.
- **Rollback**: the timestamped `VACUUM INTO` backup is the operable rollback and the one to
  document for users. `engram admin rebuild` is the *architectural* rollback for projection
  corruption, and unlike in the fact-semantics spec's caveat, it ships in this lane, in this wave.
- **The live store is never touched.** The measurement procedure in `spec-fact-semantics.md` §4.1
  Phase 0 (stop the daemon, `VACUUM INTO` a snapshot, `ENGRAM_STORE=<tmp>`, run from
  `target/debug`) is the only sanctioned way to see real numbers, it is operator-run, and it is
  never automated or put in a test.

---

## 5. Test plan

Hard constraint 1 governs everything here. **No test may open the live store**, and R14 pins the
mechanism: `ENGRAM_STORE` set to a `TempDir`, never `HOME` tricks.

### 5.0 Two isolation harnesses, and the reason there are two

**In-process (`engram-storage` unit tests).** These construct a store directly. Two belts:

```rust
// crates/engram-storage/src/test_support.rs  (#[cfg(test)] + pub(crate))
/// A TempDir, ENGRAM_STORE pointed at it, and the process-global env lock held
/// for the test's lifetime. Env vars are process-wide and cargo runs tests
/// threaded — the lock is the same pattern paths.rs:106-109 already uses, and
/// it must be poison-tolerant for the same reason.
pub(crate) fn store_guard() -> (tempfile::TempDir, StoreGuard);
/// An opened store inside a fresh guard: `SqliteStore::open(tmp.path().join("db.sqlite"))`.
pub(crate) fn temp_store() -> (tempfile::TempDir, StoreGuard, SqliteStore);
```

The explicit path is the first belt; `ENGRAM_STORE` is the second, so a helper that reaches for
`paths::db_path()` cannot escape either. `open_in_memory()` stays available for tests that need no
file, but **every migration test uses a file store** — `VACUUM INTO` needs a directory, and the
"never back up `:memory:`" rule is only testable against both.

**Spawned binary (`crates/engram-cli/tests/wave2_*.rs`).** One helper per file, `cargo_bin` called
in exactly one function per file (repo precedent: `wave05_fact_batch.rs:17-36`):

```rust
fn engram(store: &std::path::Path) -> Command {
    let mut cmd = Command::cargo_bin("engram").expect("binary built");
    cmd.env("ENGRAM_STORE", store);              // R14: this alone. No HOME tricks.
    cmd.env("ENGRAM_BENCH_FORCE_STUB", "1");
    for k in ["GEMINI_API_KEY", "COHERE_API_KEY", "OPENROUTER_API_KEY",
              "ENGRAM_FACT_LLM_STUB", "ENGRAM_FACT_WRITE_FAIL", "ENGRAM_FACT_QUEUE_ALL"] {
        cmd.env_remove(k);
    }
    cmd
}
```

Review check, and it is a real one: `rg -n 'cargo_bin' crates/engram-cli/tests/wave2_*.rs` shows
exactly one hit per file, and `rg -n 'env\("HOME"' crates/engram-cli/tests/wave2_*.rs` shows none.

### 5.1 The v9 fixture — build it BEFORE bumping `CURRENT_VERSION`

The migration tests need a real v9 store, and after `CURRENT_VERSION` becomes 10 the tree can no
longer produce one. **Implementation step zero**, before any other change:

1. On the unmodified tree, `ENGRAM_STORE=$(mktemp -d) cargo run -- remember …` a seed corpus:
   ≥3 memories, ≥8 facts including one superseded pair, one fact with `valid_from` later than its
   `superseded_at` (the clamp case), ≥4 queue rows — two resolved with opposite verdicts, two open
   sharing one semantic key (the collapse case), one open row whose facts are then deleted (the
   orphan case), and one `fact_extraction_log` row.
   **The orphan case cannot be seeded through the CLI**: `fact_conflict_queue` carries
   `ON DELETE CASCADE` on both fact FKs (`sqlite.rs:1107-1108`) and `open()` sets
   `PRAGMA foreign_keys = ON`, so deleting the facts takes the queue row with them — that cascade
   IS the brief's headline defect. Seed it by hand into the dump:
   `PRAGMA foreign_keys=OFF; DELETE FROM facts WHERE id IN (…); PRAGMA foreign_keys=ON;`, with a
   comment saying why. The same note answers "how do orphans exist in the wild": a store written
   before the FK landed, or one repaired with foreign keys off. If the fixture cannot hold one,
   test 39 and step 4's `queue_rows_skipped_orphan` counter are untested code.
2. `sqlite3 <store>/db.sqlite .dump > crates/engram-storage/tests/fixtures/v9_store.sql`.
3. Commit the fixture with a header comment naming the generation command and each seeded case, so
   a future agent can regenerate it rather than reverse-engineer it.

Tests load it with `conn.execute_batch(include_str!("fixtures/v9_store.sql"))` into a TempDir file
and then call `SqliteStore::open` on it, which triggers the real migration.

### 5.2 Test inventory

`engram-core`, pure, no store (in-module `#[cfg(test)]`):

| # | Name | Asserts |
|---|---|---|
| 1 | `registry_v1_table_is_pinned` | every relation named in interfaces v2 §3.1 maps to its stated `RelationSpec` |
| 2 | `unknown_relations_default_to_set_valued_mutable_state_text` | the safe default — a wrong `Set` costs a coexisting fact, a wrong `Single` retires a true one |
| 3 | `member_identity_canonicalizes_internally` | `member_identity("is_allergic_to", x) == member_identity("allergic_to", x)` [R6/CM-11] |
| 4 | `member_identity_v1_table` | `empagliflozin 12.5 mg daily`→`empagliflozin`; `penicillin (severe)`→`penicillin`; `hypertension stage 2`→`hypertension`; `type 2 diabetes` unchanged; `metformin xr` ≠ `metformin` |
| 5 | `predicate_arity_is_the_legacy_projection` | `allergic_to` → `Multi` — the polypharmacy defect written down as a test |
| 6 | `no_registry_entry_is_instant_and_mutable` | the assumption `interval.rs`'s point algebra rests on |
| 7 | `overlaps_is_half_open_and_points_overlap_nothing` | `[a,b)`; `[t,t]` overlaps nothing including itself |
| 8 | `close_at_clamps_to_the_old_intervals_start` | `close_at(old, s) == max(old.from, s)` |
| 9 | `may_close_refuses_heuristics_on_safety_relations` | and refuses `SuccessorInterval` on `Immutable` |
| 10 | `interval_of_tolerates_a_null_valid_from` | a hand-restored v9 backup must not panic |
| 11 | `object_pair_hash_is_order_independent` | plus encode/decode round-trip, and a value containing `\u{1f}` decodes unambiguously or returns `None` |
| 12 | `content_hash_is_stable_and_whitespace_sensitive` | memoization must not treat a reformat as identical |

`engram-storage`, temp store:

| # | Name | Asserts |
|---|---|---|
| 13 | `append_event_assigns_ascending_seq_and_unique_ids` | + `events_for_subject` returns seq order |
| 14 | `canonical_json_sorts_keys_recursively` | stable bytes independent of construction order |
| 15 | `a_rolled_back_transaction_leaves_no_event` | invariant 1, from the failure side |
| 16 | `insert_fact_appends_fact_observed_with_its_scope` | kb/diary land on the event columns |
| 17 | `close_fact_interval_sets_superseded_by_only_when_superseded` | the compat view [R2] |
| 18 | `supersede_fact_with_caller_is_not_recorded_as_heuristic` | CW-4 regression; the §5.3 audit query depends on it |
| 19 | `an_already_closed_interval_cannot_be_superseded_again` | site `:4119` |
| 20 | `newest_caller_decision_beats_a_newer_machine_decision` | the precedence, in the SQL |
| 21 | `void_decisions_are_never_selected` | |
| 22 | `a_failed_decision_lookup_propagates` | drop/rename `fact_decisions`, assert `Err`, never `Ok(None)` |
| 23 | `queue_insert_distinguishes_duplicate_pair_from_duplicate_key` | all three `InsertOutcome`s |
| 24 | `a_lost_conflict_claim_returns_none_not_success` | the `facts resolve` race, at the storage layer |
| 25 | `derivation_is_memoized_by_content_and_extractor` | same content+extractor = hit; edited content = miss; edit-and-revert = hit |
| 26 | `resolve_handle_answers_every_kind_and_a_bare_uuid` | chunk→`memory_id`; soft-deleted memory still resolves |
| 27 | `soft_delete_closes_intervals_instead_of_deleting_facts` | brief defect `sqlite.rs:1855-1869` |
| 28 | `closing_a_memorys_facts_keeps_its_adjudication_rows` | the FK-cascade defect (`edit.rs:190` → `sqlite.rs:1925` → `:1106-1108`) |
| 29 | `a_stopped_interval_is_not_active_anywhere` | the §2.9 sweep, across `get_active_facts`, `list_facts*`, `count_facts` |

Migration (`crates/engram-storage/tests/migration_v10.rs`, file store + fixture):

| # | Name | Asserts |
|---|---|---|
| 30 | `v9_store_migrates_to_v10_with_every_object` | columns, both `facts` indexes, `fact_decisions`, `derivations`, `events`, and `idx_fcq_open_key` present |
| 31 | `the_backup_is_written_before_the_transaction` | file matches `db.sqlite.pre-v10-backup.\d{14}`, opens as SQLite, and still reports `schema_version = 9` |
| 32 | `no_backup_for_memory_stores_fresh_stores_or_second_opens` | three cases, one test |
| 33 | `a_failed_backup_aborts_the_migration` | read-only store dir → `Err`, store still v9, no partial DDL |
| 34 | `the_clamp_prevents_negative_length_intervals` | the seeded `valid_from > superseded_at` row migrates to `valid_until == valid_from` |
| 35 | `resolution_maps_to_close_reason_and_authority` | the §4.2 table, parameterized |
| 36 | `resolved_queue_rows_become_decisions_with_inferred_verdicts` | `reject_new` / `close_old` / `keep_both` + the authority map |
| 37 | `open_queue_rows_get_conflict_opened_events` | without which invariant 3 fails on any backlog [IN-12] |
| 38 | `duplicate_open_keys_collapse_before_the_unique_index` | survivor = lowest `created_at`; losers `superseded_question` + a `void` decision carrying `collapsed_into`; index creation succeeds |
| 39 | `orphaned_queue_rows_stay_open_with_a_null_key` | and are counted, not invented |
| 40 | `migrating_twice_changes_nothing` | full-table digests + `COUNT(*) FROM events` identical |
| 41 | `bench_personal_json_is_unchanged_by_the_migration` | seeded store, before/after, byte-diff |

Rebuild (`crates/engram-cli/tests/wave2_admin_rebuild.rs`, spawned binary):

| # | Name | Asserts |
|---|---|---|
| 42 | `rebuild_reproduces_the_projection_byte_for_byte` | digest of `facts` (minus `embedding`/`embed_model`), `fact_conflict_queue`, `fact_decisions`, `derivations` before == after |
| 43 | `rebuild_restores_an_open_conflict_queue_after_migration` | the property IN-12 exists for |
| 44 | `rebuild_kb_touches_only_that_kb` | rows in the other KB are bit-identical |
| 45 | `dry_run_writes_nothing_and_needs_no_confirm` | db file digest unchanged |
| 46 | `rebuild_without_confirm_exits_3_and_prints_the_command` | exit-code taxonomy, and the reply is runnable |
| 47 | `rebuild_names_what_it_did_not_rebuild` | `chunks`, `memories`, `claims`, `entities` appear in `skipped` with reasons |
| 48 | `an_unknown_event_kind_is_an_error_not_a_silent_skip` | inject a row with `kind='memory.exploded'` |

Export/import v3 (`crates/engram-cli/tests/wave2_export_v3.rs`):

| # | Name | Asserts |
|---|---|---|
| 49 | `export_v3_round_trips_into_a_fresh_store` | facts/conflicts/decisions/events digests equal |
| 50 | `importing_the_same_v3_file_twice_is_idempotent` | `events_skipped_duplicate == n`, no duplicate rows |
| 51 | `a_v2_file_imports_with_a_synthesized_journal` | and the resulting store passes test 42's rebuild identity |
| 52 | `a_future_schema_version_is_refused` | `Migration` error, message says upgrade not downgrade |

Isolation (`crates/engram-cli/tests/wave2_journal.rs`, one test):

| # | Name | Asserts |
|---|---|---|
| 53 | `the_binary_writes_only_inside_engram_store` | after a full remember/recall/admin-rebuild cycle, `<tmp>/db.sqlite` exists and a canary path under the default `ProjectDirs` data dir has an unchanged mtime |

### 5.3 The safety audit query, run as a test

`spec-fact-semantics.md` §4.1 Phase 3's assertion, executed against the migrated fixture and again
after a rebuild — this is the reason `close_authority` is a column at all:

```sql
SELECT COUNT(*) FROM facts
 WHERE close_authority = 'heuristic'
   AND predicate IN ('takes_medication', 'allergic_to', 'diagnosed_with', 'blood_type');
```

The migration itself may produce non-zero here — v9 rows closed by the old classifier are history
and are honestly labelled `heuristic`. What must be zero is the count of rows this lane's code
paths close: the assertion is "no fact whose `closed_by` names an event appended after the
migration has `close_authority='heuristic'` on a safety relation". Test 18 is its unit-level twin.

### 5.4 Gates before each commit (constraint 5)

`cargo build --workspace` · `cargo clippy --workspace --all-targets -- -D warnings` ·
`cargo fmt --check` · `cargo test --workspace` · `ENGRAM_STORE=$(mktemp -d) cargo run -- bench
personal --json` diffed against the pre-change output. No `#[ignore]`d tests in this lane: every
property above is provable in wave 2 without another lane, which is why the journal is wave 2.

---

## 6. cross_lane_patches

One-liners this lane needs in files it does not own. Each is written to be applied verbatim by the
lead at wave integration [R15/IN-30]. Nothing here is a design question; the design questions are
in §7.

| # | File (owner) | Patch |
|---|---|---|
| CLP-1 | `crates/engram-cli/src/commands/mod.rs` (**lead**) | insert `pub mod admin;` alphabetically, before `pub mod bench;` (`:3`) |
| CLP-2 | `crates/engram-cli/src/cli.rs` (**contract-codegen**) | add `Admin(#[command(subcommand)] AdminCommand)` to `Command`, and `pub enum AdminCommand { /// Drop and replay the fact and adjudication projections from the journal. Rebuild { #[arg(long)] kb: Option<String>, #[arg(long)] dry_run: bool, #[arg(long)] confirm: bool } }` |
| CLP-3 | `crates/engram-cli/src/lib.rs` (**daemon-jobs**) | dispatch arm: `Command::Admin(sub) => match sub { AdminCommand::Rebuild { kb, dry_run, confirm } => commands::admin::rebuild(&ctx, kb, dry_run, confirm) },` |
| CLP-4 | `crates/engram-cli/src/error.rs` (**contract-codegen**) | the `StorageError → ErrorCode` arm reads structure, not strings: `e.is_busy() → SqliteBusy`, `NotFound(_) → NotFound`, `Migration(_) → Config`, `Serde(_) \| MalformedRow(_) → MalformedJson`, `_ → Transient` [R11] |
| CLP-5 | `crates/engram-cli/tests/cli.rs` (**lead**) | `point_at_home` (`:15-20`) gains `cmd.env("ENGRAM_STORE", home);` — one line, R14, wave 2. The four XDG/HOME lines stay for the legacy tests that assert on XDG paths |
| CLP-6 | `crates/engram-cli/src/fact_write.rs:1214` (**fact-semantics**) | `insert_fact_conflict` gains `decision_key`, `member_key`, `registry_version` and returns `InsertOutcome`. Wave-2 stopgap, if fact-semantics has not landed: pass `""`, `""`, `engram_core::relations::REGISTRY_VERSION` and match `InsertOutcome::Inserted`. **An empty `decision_key` is stored as SQL NULL** by the storage method, precisely so the stopgap cannot collide every row under `idx_fcq_open_key` |
| CLP-7 | `crates/engram-cli/src/fact_write.rs:901` and `:1368` (**fact-semantics**) | the two `Fact { … }` literals gain `closed_by: None, close_reason: None, close_authority: None`. `Fact` gets no `Default` impl (it has four required ids), so this is explicit and mechanical. The other five literal sites (`sqlite.rs:5397,:5557,:5892`, `interval.rs:205`) are inside this lane |
| CLP-9 | `crates/engram-cli/src/commands/repair.rs:18-19` (**frozen — lead**) | print the new `facts_from_deleted_memories_closed` beside the existing `facts_from_deleted_memories_removed` (which stays, now always 0). Two lines, additive, no key renamed (§2.9b) |
| CLP-8 | `crates/engram-storage/Cargo.toml` (**lead**) | `[dev-dependencies]` `tempfile = "3"` — the crate has no dev-dependencies today and every storage test needs a TempDir. Promoting `tempfile` to `[workspace.dependencies]` alongside `assert_cmd` is the tidier form and this lane has no preference |

Explicitly **not** needed, stated so nobody adds it: no `blake3` in `[workspace.dependencies]`
[R9 rejects it; `uuid` already carries `v5` at `Cargo.toml:81`], and no `#[deprecated]` attribute
on `delete_facts_for_memory` — a deprecation warning fails `clippy -D warnings` in the lane that
still calls it, so the rename ships as a plain alias plus a doc comment until write-surfaces
lands `edit.rs:190`.

Wave-3 handoffs (not patches — real work in another lane's file):

- **write-surfaces** wires `commands/export.rs` (`"version": 2` at `:28`) and `commands/import.rs`
  to `export_v3` / `import_v3` / `import_v2_with_genesis` [CD-9].
- **write-surfaces** changes `edit.rs:190` to `close_facts_for_memory`.
- **read-surfaces** takes the four remaining open-fact sites (`facts.rs:123,156,157`,
  `recall.rs:310`) at the named wave-3 gate [CD-2], and moves `facts resolve`
  (`facts.rs:451-517`) onto `resolve_conflict_with_decision` so the claim race closes and the
  caller decision is recorded [CD-3].
- **fact-semantics** implements `impl DecisionLedger for SqliteStore` over `lookup_decision`, and
  `SemanticKey::for_pair` delegating to `engram_core::keys` (§7 OQ-1).

---

## 7. Risks and open questions

**OQ-1 (blocking, needs a lead ruling before the migration is written) — where the semantic-key
encoding lives.** interfaces v2 §0 puts `SemanticKey` in `engram_graph`, and R9 pins
`SEMANTIC_KEY_NAMESPACE` there too. But the v9→v10 migration keys ~14.5k decision rows and lives
in `engram-storage`, which cannot depend on `engram-graph` — the identical argument R1 used to
move the registry into `engram-core`. Left unresolved, the workspace gets two key encodings that
agree on the day they are written, and every decision migrated under one and looked up under the
other silently stops replaying. **This spec's proposal**, implemented as §2.1's `keys.rs` unless
the lead rules otherwise: the *primitives* (`SEMANTIC_KEY_NAMESPACE`, `SEMANTIC_KEY_VERSION`,
`object_pair_hash`, `semantic_key_encode/decode`) live in `engram_core::keys`; the *type*
`SemanticKey` and `for_pair(&FactView, &FactView)` stay in `engram_graph` and delegate. One
encoding, one namespace uuid, no ownership change to the typed API. Also needs pinning: the pair
components are the two facts' `object_norm`s (`spec-fact-semantics.md` §2.3.2 line 465), not their
member identities — R9 says "a_norm"/"b_norm" without saying which.

**OQ-2 — three additive payload fields, for the lead to record in interfaces v2.1.** The rebuild
property (invariant 3) is not reachable without them: `fact.observed` needs `fact_type` and
`diary` (otherwise every rebuilt fact returns as `assertion`), `conflict.opened` needs `suggested`
and `note` (both are queue columns every conflicts listing reads), `decision.recorded` needs an
optional `collapsed_into` so `status='superseded_question'` is rebuildable rather than inferred
from note text. All three are additive and the contract says readers ignore unknown fields, so
this lane ships them and reports; none changes a pinned field's meaning.

**OQ-3 — the rebuild invariant is narrower than its one-line statement.** Invariant 3 says
`admin rebuild` "reproduces byte-identical projection state". This spec scopes that to the fact
and adjudication projections and excludes, by name and with reasons printed on every run: fact
embeddings (a recomputable blob that has no business in a JSON payload), `memories`/`chunks`
(access telemetry and chunk embeddings are deliberately not journaled), and the compiler caches.
The alternative — journaling embeddings — would multiply the journal's size by ~3 KB per chunk
and make export v3 unusable. The lead should confirm the narrowing, because the released wording
of the guarantee depends on it.

**OQ-4 — `supersede_fact_with`'s authority is derived from its `resolution` string (CW-4).** The
alternative was a new parameter and five call-site edits across two lanes. Deriving it keeps the
signature and fixes the real defect (caller decisions stamped `heuristic`, which would make the
§5.3 safety audit report the opposite of the truth), at the cost of promoting `resolution` tags to
a safety-relevant vocabulary. If the lead prefers explicitness, the change is
`supersede_fact_with_authority(old, new, resolution, valid_until, authority)` plus one line at
each of `facts.rs:480,:492,:630` and `fact_write.rs:1047,:1143,:1150,:1155`.

**OQ-5 — the v9 fixture must be captured before `CURRENT_VERSION` is bumped** (§5.1). After the
bump the tree cannot produce a v9 store, and the migration's entire test suite depends on one.
This is the single sequencing mistake in this lane that is expensive to undo, so it is
implementation step zero.

**OQ-6 — `forget` changes observable behaviour, on purpose.** `soft_delete_memory` stops deleting
the memory's facts and closes their intervals with `retracted_source` instead (the brief's
`sqlite.rs:1855-1869` defect). Facts therefore survive a `forget` as closed intervals and remain
visible in history listings. That is move 1 working as designed and constraint 3 permits it (no
field is renamed or retyped), but it is a value change on a published surface and needs a CHANGELOG
line alongside the `suggested_action` and `fact_type` entries fact-semantics is already writing.

**OQ-7 — the backup aborts the upgrade when it cannot be written.** `VACUUM INTO` transiently
needs disk equal to the store, so a large store on a full disk turns an upgrade into a hard
failure where 0.3.1 would have succeeded. This spec chooses abort over "migrate anyway", because
the migration rewrites every fact row and there is no other rollback. An `ENGRAM_SKIP_V10_BACKUP`
escape hatch is deliberately **not** specified — an env var that disables the only rollback is a
footgun with a nice name — but the lead may want one for CI images; if so it belongs in
interfaces, not in this lane's judgement.

**OQ-8 — the fourteen-site sweep is inert in wave 2 and load-bearing in wave 3.** Until
fact-semantics emits `close_reason='stopped'`, `valid_until` is only ever set together with
`superseded_by`, so §2.9's predicate is provably equivalent to the old one. The risk is the
inverse: if fact-semantics lands and read-surfaces' four sites slip past the wave-3 gate, a
stopped medication reads as current on `facts.rs` and `recall.rs` while storage is already
correct — a divergence between two surfaces is worse than a uniform bug. The gate is named in
§6's handoff list and this lane's report repeats it.

**OQ-9 — migration cost.** ~30k events in one transaction on the measured backlog. The budget is
under 30 s for a 100k-row store; the fix if it is exceeded is statement hoisting and
`PRAGMA synchronous=OFF` for the migration transaction only. Splitting the transaction is not an
option: a half-migrated store has no honest version number, and `open()` would migrate it again
on the next command.

**OQ-10 — two extraction markers coexist.** `fact_extraction_log` (memory-keyed) still drives
`PENDING_EXTRACTION_PREDICATE:735`, while `derivations` (content-keyed) is what makes
re-derivation memoized. This lane writes both and backfills the second from the first. Collapsing
the pending predicate onto the derivation ledger — which is what actually fixes the brief's
"backfill pending predicate is marker-only, contradicting agent-info" defect — is a read/daemon
surface change and is deliberately out of scope here.

**OQ-11 — `resolve_handle`'s probe order across eight tables** is a linear scan of eight indexed
point lookups per bare-uuid call. Every mutator does this once per invocation, so it is not a hot
path; if it ever becomes one the fix is a `handles(id, kind)` projection table fed by the journal,
which the journal makes cheap. Not built now, because a table that can drift from its sources is
a worse bug than eight index probes.

**OQ-12 — `facts.source_memory_id … ON DELETE CASCADE` survives this lane** (`SCHEMA_V1:483`).
With `soft_delete_memory` and `delete_facts_for_memory` rewritten to close intervals (§2.9b), the
only remaining path that deletes a memory ROW is `hard_delete_memory:2085-2091` (ingest rollback),
where the cascade is wanted. Dropping the FK means a full table rebuild of the largest table in
the store, inside the migration that already rewrites every fact row, for one caller that wants
the current behaviour. Recorded rather than done; the lead should confirm that "never hard-delete
a fact" is scoped to user-visible deletes and does not include rollback of a write that never
became visible.

**OQ-13 — this lane and write-surfaces disagree about who stamps the close reason on `edit` and
`forget`, and with what.** This spec has the storage methods stamp `retracted_source` +
`explicit_end` (the memory went away; no successor exists, no caller adjudicated a pair).
`spec-write-surfaces.md` has the CLI stamp `corrected` + `caller` for `edit` and `caller` for
`forget`. Both land in wave 2/3 and both are defensible: the caller did ask, but the caller did
not adjudicate a fact pair, and `close_authority='caller'` is the value the §5.3 safety audit
treats as "a human decided this". Interfaces v2 assigns neither. Lead ruling needed before either
lane's tests are written; this spec's preference is storage-stamped `retracted_source`, with
`authority = Caller { actor }` when the close came from an interactive command, because that keeps
the reason honest and the authority accurate at the same time.

**OQ-14 — `PRAGMA synchronous=OFF` (§4.6) has no written recovery story.** If the migration is
the escape hatch's only user and the machine loses power mid-transaction, SQLite's rollback
journal may not be durable and the store can be left corrupt rather than merely un-migrated. The
pre-v10 backup exists, but nothing tells the user how to use it: `open()` will keep trying to
migrate the corrupt file on every command, and the restore is a manual `mv`. If the lead keeps the
escape hatch, it needs a CHANGELOG-level restore procedure and, ideally, `engram doctor` noticing
a `db.sqlite.pre-v10-backup.*` sibling next to an unreadable store. The alternative — never
lowering `synchronous` — costs only time on a store nobody has measured yet.

**OQ-15 — tests 42–48 cannot run until CLP-1/2/3 land.** `admin rebuild` has no binary surface in
this lane: the subcommand (`cli.rs`), the dispatch arm (`lib.rs`) and `pub mod admin;`
(`commands/mod.rs`) all belong to other owners. The library entry point
`rebuild_projections(kb, dry_run)` is fully testable in wave 2, so the rebuild-identity property
(test 42) is proved in `engram-storage` and only the CLI-envelope assertions (tests 45–47) wait on
integration. Written down so nobody reads a red spawned-binary suite as a broken rebuild.

**OQ-16 — the CLP-6 stopgap leaves wave-2 queue semantics half-migrated.** With `decision_key`
passed as `""` → SQL NULL until fact-semantics lands, `idx_fcq_open_key` matches nothing,
`InsertOutcome::DuplicateKey` never fires on the production path, and the open-queue storm behaves
exactly as it does today. That is the intended trade (wave 2 must be green alone), but it means
tests 23, 37 and 38 must construct their keys at the fixture level rather than through
`fact_write.rs`. "Wave 2 green" is not "the storm is fixed"; the storm closes in wave 3.

**OQ-17 — interfaces v2 §2.1 step 2 names a column that does not exist.** It reads
"`resolution` → `close_reason` and `resolved_by` → `authority`", but `facts` has no `resolved_by`
(that column is on `fact_conflict_queue`); `types.rs:184` and the DDL carry `resolution` only. The
annex maps both outputs from `resolution`, and §4.2 above follows the annex. Flagged so the next
reader of R5 does not spend an hour looking for the missing column, and so the lead can correct
the wording in interfaces v2.1.

**OQ-18 — a soft-deleted memory resolves (§2.7) but nothing yet reads its scope.** `Resolved`
deliberately carries no `deleted` flag, so a caller that wants "already deleted" rather than "not
found" must read the memory row — including for a row whose `deleted = 1`, which several existing
read helpers filter out. This lane provides `resolve_handle` and no more; if write-surfaces needs
`kb`/`diary` of a deleted memory to render its error, that is a one-method lane request, not a
re-derivation.

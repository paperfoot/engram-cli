# interfaces.md — the shared contract for arch/v0.4

Cross-lane ground truth. Lanes code against THIS; if your spec needs something not here, flag it in your report — do not invent a divergent shape. Additions land here via the integration lead, never by editing from a lane.

## 1. Handles

```
handle := kind ":" uuid | uuid            # bare uuid = legacy input, always accepted
kind   := mem | chunk | fact | conflict | entity | doc | job | event
```

```rust
// engram-core
pub enum HandleKind { Memory, Chunk, Fact, Conflict, Entity, Document, Job, Event }
pub struct Handle { pub kind: Option<HandleKind>, pub id: Uuid }
impl Handle { pub fn parse(s: &str) -> Result<Handle, HandleParseError>; pub fn render(kind: HandleKind, id: Uuid) -> String; }
```

Resolution lives in storage:

```rust
pub struct Resolved { pub kind: HandleKind, pub id: Uuid, pub memory_id: Option<Uuid> }
impl SqliteStore { pub fn resolve_handle(&self, h: &Handle) -> Result<Option<Resolved>, StorageError>; }
```

Rules: a mutator that needs a memory id accepts ANY handle; if it resolves to a chunk, use `memory_id`; if it resolves to a different kind, return `wrong_id_kind` naming the actual kind and the correct handle. A bare uuid is looked up across kinds (memories first, then chunks, facts, conflicts, entities, documents). Unknown → `not_found`, exit 3 — on every mutator including `forget`.

Output rule: every serialized result row carries `id` (legacy, unchanged meaning) AND, where applicable, `memory_id` plus `handle` (kind-prefixed). Additive only.

## 2. The journal

```sql
CREATE TABLE events (
    seq        INTEGER PRIMARY KEY AUTOINCREMENT,   -- total order
    id         TEXT NOT NULL UNIQUE,                -- uuid
    ts         TEXT NOT NULL,                       -- rfc3339, transaction time
    actor      TEXT NOT NULL,                       -- "cli:<diary>" | "daemon" | "job:<uuid>"
    kind       TEXT NOT NULL,
    subject_id TEXT,                                -- primary entity uuid
    kb         TEXT,
    diary      TEXT,
    payload    TEXT NOT NULL                        -- canonical JSON, schema per kind below
);
CREATE INDEX idx_events_subject ON events(subject_id, seq);
CREATE INDEX idx_events_kind    ON events(kind, seq);
```

Event kinds v1 (payload fields in parentheses; all payloads also allowed extra fields — readers ignore unknowns):

- `memory.written` (content, content_hash, importance, tags, event_time, source)
- `memory.revised` (memory_id, content, content_hash, prev_content_hash)
- `memory.retracted` (memory_id, reason?)            — replaces hard behavior of forget
- `memory.meta_changed` (memory_id, importance?)
- `derivation.ran` (memory_id, content_hash, deriver, model?, output_fact_ids, fact_count) — deriver is "fact_extract@<version>"; replaces fact_extraction_log semantics
- `fact.observed` (fact_id, subject, subject_norm, relation, object, object_norm, valid_from, valid_until?, confidence, source_memory_id, content_hash)
- `fact.interval_closed` (fact_id, valid_until, closed_by_fact?, reason: "superseded"|"stopped"|"corrected"|"retracted_source")
- `fact.confirmed` (fact_id)
- `conflict.opened` (conflict_id, new_fact_id, old_fact_id, detector, rule, score?)
- `decision.recorded` (decision_id, semantic_key, verdict, conflict_id?, note?, actor)
- `decision.replayed` (decision_id, applied_to_conflict?, new_fact_id, old_fact_id)
- `compile.ran` (kb, job_id?, stats)
- `job.submitted` / `job.completed` / `job.failed` (job_id, kind, params/stats/error)

Invariants:
1. Every projection mutation happens in the SAME transaction as its event append. No projection write without an event; no event without its projection effect.
2. Nothing is ever deleted from `events`.
3. `engram admin rebuild [--kb K]` drops and replays projections from the journal and must reproduce byte-identical projection state (property-tested).
4. Export v3 = journal slice + current schema version. Import v3 = replay. v2 import stays supported (synthesizes genesis events).

```rust
// engram-storage
pub struct EventDraft { pub kind: EventKind, pub subject_id: Option<Uuid>, pub kb: Option<String>, pub diary: Option<String>, pub actor: String, pub payload: serde_json::Value }
impl SqliteStore {
    /// Append inside the caller's open transaction. Returns assigned seq + id.
    pub fn append_event(&self, draft: &EventDraft) -> Result<(i64, Uuid), StorageError>;
    pub fn events_for_subject(&self, id: Uuid) -> Result<Vec<EventRow>, StorageError>;
}
```

Migration v9→v10: timestamped `db.sqlite.pre-v10-backup` first; genesis events synthesized per existing row (`memory.written` from memories incl. deleted ones followed by `memory.retracted`, `fact.observed` from facts, `conflict.opened`+`decision.recorded` from resolved queue rows, `derivation.ran` from fact_extraction_log); idempotent; `bench personal` unchanged.

## 3. Interval facts

`facts` table keeps its columns; adds `closed_by TEXT` (fact/event uuid) and `close_reason TEXT`. Compatibility view: `superseded_by`/`superseded_at` remain populated when `close_reason='superseded'` so existing readers keep working.

```rust
pub enum Arity { SetValued { safety: bool }, SingleValued { safety: bool } }
pub fn relation_arity(canonical: &str) -> Arity;
// takes_medication / allergic_to / diagnosed_with => SetValued { safety: true }
// blood_type, birthday, died_on, nationality      => SingleValued { safety: blood_type only }
// unknown predicates                              => SetValued { safety: false }  (unchanged default)
pub fn member_identity(relation: &str, object_norm: &str) -> String;
// e.g. "empagliflozin 12.5 mg daily" -> "empagliflozin"; v1 heuristic is fact-semantics lane's call, must be pure + versioned
```

Write semantics (replaces classify-then-queue for set-valued relations):
- New observation, set-valued, new member identity → coexist. NO conflict row.
- Same member identity, same attributes → confirmation.
- Same member identity, different attributes (dose change) → close old interval (`reason: superseded`) if non-safety; if safety, open a conflict whose `suggested_action` is `requires_context` (NEVER `supersede-old`).
- Explicit negation ("stopped X") → interval close (`reason: stopped`), safety-gated the same way.
- Single-valued relations keep today's recency/confidence logic, expressed as interval closes.
- Decision replay runs BEFORE conflict opening: identical `semantic_key` with a recorded verdict applies silently and appends `decision.replayed`.

```rust
pub struct SemanticKey { pub subject_norm: String, pub relation: String, pub object_pair_hash: String }
// object_pair_hash = hex(blake3(min(a_norm,b_norm) || "\x1f" || max(a_norm,b_norm)))
```

## 4. Errors and envelope

```rust
pub enum ErrorCode { BadInput, NotFound, WrongIdKind, Config, Transient, RateLimited, SqliteBusy, ConflictAlreadyResolved, MalformedJson }
```
Exit mapping: BadInput/NotFound/WrongIdKind/MalformedJson→3, Config→2, Transient/SqliteBusy→1, RateLimited→4, ConflictAlreadyResolved→3. Every error carries a non-boilerplate `suggestion`. Clap parse failures are intercepted (`try_parse`) and emitted through the JSON envelope, exit 3.

Envelope additions (all additive):
- Every listing: `returned`, `total`, optional `cursor`; legacy `count` stays = returned.
- Every read: `metadata.scope { kbs, diary, chunks_considered }`; `data.verdict: "results" | "no_match"`; on no_match: `data.absence { top_score, threshold, searched_kbs, searched_chunks }`.
- `edit`/`memory update` response: `facts_invalidated: usize`, `reextraction: "replayed" | "pending" | "none"`, plus job handle when async re-derivation was submitted.
- `elapsed_ms` measured once in dispatch for every command.
- `resolve_command` fields are replaced by `resolve { program: "engram", args: [...], allowed_actions: [...] }`; the old string field stays but contains ONE runnable command with no pipe characters.

## 5. Store resolution

Precedence: `--store <dir>` flag > `ENGRAM_STORE` env > ProjectDirs default. `engram agent-info`, `--help`, and clap errors must not open, create, or migrate any store. All tests set `ENGRAM_STORE` to a TempDir (never rely on HOME tricks again).

## 6. Single-writer core and jobs

```rust
// engram-cli (core module) — daemon-jobs lane owns; other lanes only consume
pub struct Core { /* store + single writer */ }
impl Core {
    pub fn write<T>(&self, f: impl FnOnce(&mut WriteTx) -> Result<T>) -> Result<T>; // ALL mutations
    pub fn submit_job(&self, kind: JobKind, params: Value) -> Result<JobHandle>;    // compile, backfill, reflect
}
```
`facts resolve` claims its queue row inside the write closure (conditional UPDATE, exactly-one-row) and appends `decision.recorded`; a lost claim returns `ConflictAlreadyResolved`. CLI and daemon share one response-builder module per command family; serve.rs may not construct bespoke response JSON.

## 7. Wave sequencing (who lands what, in order)

1. Wave 2: engram-core types (Handle, EventKind, Arity, ErrorCode) + journal table/append + genesis migration + store resolution + contract-codegen skeleton. Everything else compiles against these.
2. Wave 3: fact-semantics (intervals, arity, replay) + write-surfaces + read-surfaces, in worktrees, integrated in that order.
3. Wave 4: daemon-jobs (Core, HTTP, jobs), absorbing the response builders.
4. Wave 5: reasoning-layer (`state`, procedural kind, reflect job) on top.

Compat promise for the whole build: existing scripts and agents keep working unmodified; every legacy field keeps meaning what it meant; new behavior is additive or strictly-more-correct (exit codes, totals, suggestions).

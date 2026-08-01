# spec-write-surfaces.md — the write half of move 5, on moves 1 and 3

Lane: **write-surfaces**. Wave 3.
Owns exactly: `crates/engram-cli/src/commands/{remember,edit,forget,memory,import,export}.rs`.
Everything else it needs arrives through `docs/arch/interfaces.md` (v2, rulings R1–R15 binding) or
through the lead's patch channel (§6 `cross_lane_patches`).

Reads, in precedence order: `interfaces.md` v2 (overrides) → `redesign-brief.md` →
`coherence-report.md` (CD-9, CW-4, CC-1, CC-5, IN-7, IN-26) → `spec-fact-semantics.md` §6.7
(the `SourceKind::UserStated` request this lane accepts).

---

## 1. Scope and non-goals

### 1.1 What this lane ships

| # | Deliverable | Defect it kills |
|---|---|---|
| W1 | Every mutator (`edit`, `forget`, `memory update`, `memory delete`, and the id-bearing paths of `remember`) accepts ANY `Handle` kind: `chunk:` follows `memory_id`; a wrong kind returns typed `wrong_id_kind` naming the actual kind and the correct handle; an unknown uuid returns `not_found`. Both exit 3. | agent feeds a recall chunk id to `edit`, gets `no memory found … (already deleted?)`, concludes a race, rebuilds the memory by hand (edit.rs:60-64) |
| W2 | `forget` on an unknown id is `not_found`, exit 3 — never today's exit-0 `{"deleted": false}` (forget.rs:17-21). An id that resolves but is already retracted stays exit 0, `deleted:false`, `already_retracted:true`. | silent success on a typo; the caller believes a memory is gone |
| W3 | `edit --content` appends `memory.revised`, and **stops calling `delete_facts_for_memory`** (edit.rs:190 → sqlite.rs:1925 + FK cascade sqlite.rs:1106-1108). Old-content facts are CLOSED as intervals (`close_reason='corrected'`), never deleted; the adjudication ledger survives the edit. | move-1 headline defect; move-3 replay is untestable while this stands (CD-3) |
| W4 | `edit` reports `facts_invalidated: <n>` and `reextraction: "replayed" \| "pending" \| "none"` in both the envelope and `metadata` (interfaces.md §4). | the re-extraction time bomb: markers hard-deleted, backfill's pending predicate is marker-only, agent-info says otherwise |
| W5 | `forget` appends `memory.retracted` and closes the memory's open facts as intervals with `close_reason='retracted_source'` — facts keep their history instead of being `DELETE`d under a soft-deleted memory (sqlite.rs:1855-1869). | forget hard-deleting facts; a "soft" delete that is hard where it matters |
| W6 | `--facts-json` (on `remember` and `memory add`) stamps `SourceKind::UserStated` — **ruling R13 / IN-26 / spec-fact-semantics §6.7**. Mixed runs (`--facts-json --extract-facts`) become two batches so provenance is never averaged. | `correction_by_user` in the classifier is dead code; no production path ever produced `UserStated` (remember.rs:632) |
| W7 | `export` emits a **v3** body = journal slice + schema version + the v2 projection sections (embeddings included); `import` dispatches v3 → storage replay, v2 → today's path, v1 → today's legacy path. Format, replay and v2-genesis synthesis are storage-journal's; this lane wires the commands (CD-9). | lossy export/import; a restored store with no journal and therefore no history |
| W8 | Every response from these commands carries `handle` (`mem:<uuid>`) beside the legacy `id`, additively. | copy-pasted ids that lose their kind |

### 1.2 Non-goals (explicitly someone else's, or later)

- **The journal itself.** `events` DDL, `append_event`, `EventDraft/EventRow`, `resolve_handle`,
  `Resolved`, `close_fact_interval`, the v9→v10 migration, `admin rebuild`, the export-v3 slice
  format and its replay: **storage-journal**. This lane calls them and specifies the call sites.
- **Contradiction detection, arity, decision replay, `safety_gate`, `allowed_actions`:**
  fact-semantics. This lane changes exactly one thing in that surface — the `source_kind` field of
  `FactWriteCtx` it already constructs.
- **`CliError` variants, `ErrorCode`, envelope rendering, `respond.rs`, clap definitions,
  `agent-info`, `skill.md`:** contract-codegen. This lane requests two variants and two flags
  through §6 and codes against them.
- **Inline re-extraction with an LLM inside `edit`.** Out of scope for wave 3: `edit` marks the
  memory pending and reports `reextraction: "pending"`. The `"replayed"` value is contracted here
  and produced from wave 4, when the content-addressed derivation ledger and the backfill job land
  (§7 open question 1).
- **`--store` flag** (R14: deferred to wave 4). Tests use `ENGRAM_STORE` only.
- **`memory list`** stays a read; it gains nothing but the `handle` field (W8).
- **`facts resolve`, `recall`, `facts list`:** read-surfaces, even where they display what this
  lane wrote.

### 1.3 Invariants this lane must not break

1. Every projection mutation happens in the SAME transaction as its event append
   (interfaces.md §2 invariant 1). Concretely: no `append_event` outside the `with_tx` closure that
   performs the effect, and no effect without its event.
2. Nothing is hard-deleted. Chunks are the one exception this lane keeps — they are a
   rebuildable index of `memories.content`, not history, and `memory.revised` carries the content
   needed to rebuild them.
3. Bare UUIDs keep working everywhere (constraint 3). Existing JSON fields keep name and type;
   `deleted`, `updated`, `chunks_replaced`, `imported`, `version` all stay.
4. Exit-code taxonomy preserved: 0 success, 1 transient, 2 config, 3 bad input / not found /
   wrong kind, 4 rate limited.
5. Paid work is never done for an id that does not exist. `edit`'s existence check stays BEFORE
   the embedder call (edit.rs:51-64) — it becomes a handle resolution, not a `memory_scope`.

---

## 2. Data model and API changes

### 2.1 What this lane implements (inside its own files)

One resolution helper, one home: **`commands/memory.rs`**. It is already `pub mod memory;` in
`commands/mod.rs`, it is this lane's file, and putting the helper there costs zero cross-lane
patches. `edit.rs`, `forget.rs` and any future id-bearing surface call it.

```rust
// crates/engram-cli/src/commands/memory.rs  (NEW, write-surfaces)
use engram_core::handle::{Handle, HandleKind, HandleParseError};
use engram_storage::Resolved;

/// A mutator's resolved target: the memory it will write, and how we got there.
pub(crate) struct MemoryTarget {
    /// The memory id every write below uses.
    pub memory_id: uuid::Uuid,
    /// `mem:<uuid>` — echoed in every response (W8).
    pub handle: String,
    /// Set when the caller passed a `chunk:` handle (or a bare uuid that turned
    /// out to be a chunk); rendered as `resolved_from` so the agent learns the
    /// mapping instead of re-deriving it.
    pub resolved_from: Option<String>,
    pub kb: String,
    pub diary: String,
    /// True when the memory row exists but `deleted = 1`. `forget` needs this to
    /// answer "already retracted" (exit 0) rather than "not found" (exit 3);
    /// `edit` turns it into `NotFound`.
    pub retracted: bool,
}

/// Resolve ANY handle kind to the memory a mutator should write.
///
/// `mem:` → itself. `chunk:` → its `memory_id`. Every other kind → `wrong_id_kind`
/// naming the kind we actually found and the handle that WOULD work. Unknown uuid
/// → `not_found`. Both exit 3. A bare uuid is resolved across kinds, unchanged
/// from v0.3's promise (interfaces.md §1).
pub(crate) fn resolve_memory_target(
    ctx: &AppContext,
    raw: &str,
    command: &str,          // "edit" | "forget" | "memory update" — used in the message only
) -> Result<MemoryTarget, CliError>;
```

Resolution algorithm, exactly. **Note the TWO-step probe**: storage's `resolve_handle` with a
prefixed handle probes ONLY that kind's table and returns `None` for a uuid that lives under a
different kind (spec-storage-journal §2.7). A single call therefore cannot tell `not_found` from
`wrong_id_kind` for a PREFIXED id, and reporting `not_found` for `edit fact:<memory-uuid>` would
reintroduce exactly the "already deleted?" guess W1 exists to kill.

1. `Handle::parse(raw)`.
   - `HandleParseError::BadUuid(s)` → `CliError::BadInput(format!("invalid UUID {raw}: {s} is not a valid UUID"))`.
     The v0.3 surfaces build their message from `uuid::Error` (`edit.rs:49`, `forget.rs:15`);
     `HandleParseError`'s own `Display` is `'{s}' is not a valid UUID` (`handle.rs:92`) and does
     NOT carry the parse error. No existing test pins the wording — `forget_with_bad_uuid_is_bad_input`
     (`cli.rs:896-900`) asserts exit 3 and the `bad_input` code only — so the requirement is
     "contains `invalid UUID`", and test 6 asserts that substring, not the whole string.
   - `UnknownKind(p)` → `BadInput("unknown id kind `{p}`: expected one of mem, chunk, fact,
     conflict, entity, doc, job, event")`. (`HandleKind::parse` also accepts the aliases `memory`
     and `document`; they never reach this arm.)
2. `ctx.store.resolve_handle(&h)?` (storage-journal §2.7, interfaces.md §1).
   - `Some(Resolved { kind: Memory, id, .. })` → target `id`.
   - `Some(Resolved { kind: Chunk, memory_id: Some(m), .. })` → target `m`,
     `resolved_from = Some("chunk:<uuid>")`.
   - `Some(Resolved { kind: Chunk, memory_id: None, .. })` → `CliError::Transient` — an orphan
     chunk is store corruption, not caller error (`engram doctor --integrity` is the hint).
   - `Some(Resolved { kind: Fact, memory_id: Some(m), .. })` → **`wrong_id_kind`, not a silent
     follow-through.** A fact id names a claim, not the memory a mutator should rewrite; the
     `correct` field carries `mem:<m>` so the agent can re-run without another round trip.
   - `Some(_)` of any other kind `k` → `wrong_id_kind` with `actual = k.as_str()`,
     `expected = "mem"`, `handle = raw`, `correct = None`.
   - `None` **and `h.kind` was `Some(_)`** → step 2b. `None` **and `h.kind` was `None`** (a bare
     uuid, already probed across every table) → `CliError::NotFound { what: command, handle: raw }`.
2b. **Second probe, prefixed ids only:** `ctx.store.resolve_handle(&Handle { kind: None, id })?`.
   - `Some(Resolved { kind: k, .. })` → `wrong_id_kind` naming `k` as `actual`, with `correct` set
     to `mem:<memory_id>` when the second probe supplied one and `None` otherwise. This is the
     case that makes `edit fact:<uuid>` and `edit mem:<chunk-uuid>` legible.
   - `None` → `CliError::NotFound { what: command, handle: raw }`.
   Note the `handle` field of `WrongIdKind` is the id the CALLER passed; `correct` is the one that
   would work. An earlier draft of this spec rendered `Handle::render(k, id)` into `handle` and
   called it "the handle that WOULD work" — it is the caller's own id re-spelled, and the
   suggestion built from it would fail identically.
3. `(kb, diary, retracted)`: `ctx.store.memory_scope(target)?` **filters `deleted = 0`**
   (`sqlite.rs:2763`, and the test at `:5992` says so in its name), so it answers `None` for
   exactly the double-`forget` case that must succeed. Requesting
   `memory_scope_including_deleted(id) -> Option<(String, String, bool)>` (WR-9) rather than
   inferring: `edit` maps `retracted = true` to `NotFound`, `forget` carries it into §3.3, and the
   `actor` string `cli:<diary>` is available on both paths. Without WR-9 there is no way to build
   a `MemoryTarget` for an already-retracted memory and W2's exit-0 half is unimplementable.

`resolve_handle` returning `Some` for a soft-deleted memory is expected and required
(storage-journal §2.7 states it explicitly): `forget` must be able to tell "unknown id" (exit 3)
from "already retracted" (exit 0).

### 2.2 What this lane needs from storage-journal (interface requests)

All in `crates/engram-storage`. Shapes below are **transcribed from `spec-storage-journal.md`**,
which is the owner; where an earlier draft of this spec invented a name, the storage name wins and
the divergence is called out. Two conflicts are NOT resolvable by transcription and are escalated
in §7 (open questions 9 and 10): who appends the memory/fact events, and what `close_reason` /
`authority` an `edit`-driven closure carries.

**Ownership warning, read before coding.** `spec-storage-journal.md` §5 puts the event append and
the fact closure INSIDE the storage methods this lane already calls: `update_memory` gains
`memory.revised` / `memory.meta_changed` (`:1897-1919`), `soft_delete_memory` gains
`memory.retracted` plus a per-fact `close_fact_interval` (`:1855-1870`), and
`delete_facts_for_memory` is rewritten to close rather than delete (`:1925-1935`). If this lane
ALSO appends those events and ALSO calls `close_facts_for_memory`, every edit and every forget
writes its events twice and closes its facts twice. Until the lead rules (§7 Q9), the binding
reading is **storage appends, this lane calls and reports** — so E4/E5 and §3.3's `append_event`
steps become assertions about what the storage call already did, and this lane's job on those
lines is to READ BACK the closed-fact ids for the envelope, not to write them.

```rust
impl SqliteStore {
    // interfaces.md §1 — already contracted, listed for completeness.
    pub fn resolve_handle(&self, h: &Handle) -> Result<Option<Resolved>, StorageError>;
    // interfaces.md §2 — appended INSIDE the caller's open transaction.
    pub fn append_event(&self, draft: &EventDraft) -> Result<(i64, Uuid), StorageError>;

    /// NEW (write-surfaces request WR-1). Close every OPEN interval derived from
    /// this memory, appending one `fact.interval_closed` per fact, and return the
    /// ids closed. Replaces `delete_facts_for_memory` at both call sites.
    /// `reason` is `CloseReason::Corrected` (edit) or `RetractedSource` (forget);
    /// `authority` is `CloseAuthority::Caller { actor }`.
    pub fn close_facts_for_memory(
        &self,
        memory_id: Uuid,
        at: DateTime<Utc>,
        reason: CloseReason,
        authority: &CloseAuthority,
        actor: &str,
    ) -> Result<Vec<Uuid>, StorageError>;

    /// NEW (WR-2). Clear ONLY the extraction marker (`fact_extraction_log`), so the
    /// memory is pending again for `facts backfill`, without touching facts. Today
    /// this is welded to fact deletion inside `delete_facts_for_memory`
    /// (sqlite.rs:1925-1935) and that welding is the re-extraction time bomb.
    pub fn clear_extraction_marker(&self, memory_id: Uuid) -> Result<bool, StorageError>;

    /// WR-3 — storage-journal §2.4 owns this and its signature is
    /// `derivation_for(content_hash, extractor_id)`: the ledger is keyed
    /// `(content_hash, extractor_id@version)`, PK at spec-storage-journal:325.
    /// An earlier draft of this spec asked for a one-argument
    /// `derivation_for_content(hash)`; that cannot answer the question, because
    /// "already derived" is only meaningful per extractor. This lane passes the
    /// extractor_id `edit` would use for a re-extraction. May land in wave 4;
    /// see §7 open question 1.
    pub fn derivation_for(&self, content_hash: &str, extractor_id: &str)
        -> Result<Option<DerivationRow>, StorageError>;

    /// NEW (WR-4). `soft_delete_memory` must STOP deleting facts
    /// (sqlite.rs:1862-1868 `DELETE FROM facts WHERE source_memory_id = ?1`).
    /// Same name, same return, minus the delete: retraction is a closure, not an
    /// erasure. `delete_derived_for_memory_chunks` stays — claims are a compiler
    /// cache, not history.
    pub fn soft_delete_memory(&self, memory_id: Uuid) -> Result<bool, StorageError>;

    /// WR-5, CD-9 — storage-journal §2.13, `crates/engram-storage/src/export_v3.rs`.
    /// `ExportV3` is a CONCRETE struct carrying events PLUS the v2 sections
    /// (kbs, documents, memories, chunks, facts), not an opaque journal slice.
    /// An earlier draft of this spec named these `export_journal_slice` /
    /// `import_journal_slice` / `synthesize_genesis_events` and treated the
    /// slice as events-only; those names do not exist and that shape drops
    /// documents and KBs on import (see §3.6).
    pub fn export_v3(&self, kb: Option<&str>) -> Result<ExportV3, StorageError>;
    pub fn import_v3(&self, doc: &ExportV3) -> Result<ImportV3Report, StorageError>;
    /// v2/v1 files carry no events; the row importers and the genesis generator
    /// run together, in storage, so genesis cannot disagree with the rows.
    pub fn import_v2_with_genesis(&self, kbs: &[KbExportRow], documents: &[DocumentExportRow],
                                  memories: &[MemoryExportRow], chunks: &[ChunkExportRow],
                                  facts: &[FactExportRow]) -> Result<ImportV3Report, StorageError>;
}
```

`ExportV3`, `ImportV3Report` and `DerivationRow` are storage-journal's types. This lane
serializes/deserializes `ExportV3` whole and reads `ImportV3Report` for the envelope; it does not
walk the events array itself, so per-KB scoping (`export_v3(Some(kb))` also emits `kb IS NULL`
subject events, spec-storage-journal:763), event-id preservation, and the
`schema_version`-ahead-of-binary refusal are all storage's, uniformly, and cannot drift between a
CLI import and a daemon one.

### 2.3 What this lane needs from contract-codegen

**Transcribed from `spec-contract-codegen.md` §2.3, which owns `error.rs`.** An earlier draft of
this spec specified different field names (`NotFound(String)`,
`WrongIdKind { message, got: HandleKind, expected: HandleKind, handle }`) and every call site in
§2.1/§3 was written against them. Contract-codegen's shapes are binding; this lane constructs:

```rust
// crates/engram-cli/src/error.rs — owner: contract-codegen (both exit 3, interfaces.md §4)
pub enum CliError {
    // …existing variants unchanged…
    NotFound { what: &'static str, handle: String },
    WrongIdKind { handle: String, actual: String, expected: String, correct: Option<String> },
}
```

`what` is a `&'static str`, so `resolve_memory_target`'s `command` parameter must be `&'static str`
too — `"edit" | "forget" | "memory update"` are literals at every call site, so this costs nothing,
but a `String` there will not compile. `actual`/`expected` are the `HandleKind::as_str()` forms
(`"fact"`, `"mem"`), not the enum: contract-codegen keeps `error.rs` free of a `engram_core::handle`
dependency. The suggestion string is built by `respond::retry_with_handle` from `correct`
(spec-contract-codegen:453, :763) — this lane supplies `correct`, never the prose.

Today every "the id names nothing" surface returns `BadInput` (edit.rs:61, :216). `NotFound` is
the same exit code with an honest `code` string, so no exit-code taxonomy is reinterpreted
(constraint 3) and agents can branch on `error.code`.

### 2.4 SQL DDL owned by this lane

**None.** Every table this lane writes (`events`, `facts`, `fact_extraction_log`,
`fact_decisions`, `memories`) is created and migrated by storage-journal (interfaces.md §2, §2.1,
IN-11). This lane adds no column and no index. The only DDL-adjacent obligation is negative and
load-bearing: **the FK cascade `fact_conflict_queue → facts` (sqlite.rs:1106-1108) must no longer
be reachable from a write surface**, which is achieved by removing the two `DELETE FROM facts`
paths this lane triggers (edit's `delete_facts_for_memory`, forget's inline delete).

### 2.5 Event payloads this lane appends

Per interfaces.md §2 v2, unchanged shapes, appended in the same transaction as the effect.

**Who appends is the §7 Q9 ruling, and the table below is the EFFECT list, not a work order.**
Under the binding reading (§2.2), storage's `update_memory`, `soft_delete_memory` and
`insert_memory_with_kb` append `memory.revised`, `memory.meta_changed`, `memory.retracted` and
`memory.written` themselves, and this lane appends nothing on those paths — it asserts they
happened (tests 13, 21, 22, 25). The earlier draft's §2.5 row for `remember`/`memory add` was
already contradicted by §3.4's "W6 only, nothing else"; §3.4 was the correct reading and this
table now says so.

| Command | Event | Payload (appended by storage unless noted) |
|---|---|---|
| `edit --content` | `memory.revised` | `{ memory_id, content, content_hash, prev_content_hash }` |
| `edit --importance` (either form) | `memory.meta_changed` | `{ memory_id, importance }` |
| `edit --content` (per closed fact) | `fact.interval_closed` | written by `close_facts_for_memory`; `reason: "corrected"`, `authority: "caller:<actor>"`, `sets_superseded_by: false` |
| `forget` | `memory.retracted` | `{ memory_id, reason: "user_forget" }` |
| `forget` (per closed fact) | `fact.interval_closed` | `reason: "retracted_source"`, `authority: "caller:<actor>"`, `sets_superseded_by: false` |
| `remember` / `memory add` | `memory.written` | `{ content, content_hash, importance, tags, event_time, source }` |
| `import` (v1/v2) | `memory.written` + genesis | via `import_v2_with_genesis` |

`actor` on every draft is `format!("cli:{diary}")` (interfaces.md §2 actor grammar).
`content_hash` is whatever `derivation.ran` uses — **`engram_core::keys::content_hash(&str)
-> String`** (spec-storage-journal:164, :351, :777), so the derivation ledger and `memory.revised`
cannot disagree about what "same content" means. WR-6 asked for it "in `engram_storage` or
`engram_core`"; `engram_core::keys` is the pinned home and this lane imports it from there.

The `authority` and `reason` values in the two `fact.interval_closed` rows above are **contested,
not settled** — spec-storage-journal:798 has `soft_delete_memory` close with
`reason="retracted_source", authority="explicit_end"`, and :800 has the rewritten
`delete_facts_for_memory` (aliased `close_facts_for_memory`) close with `retracted_source`
unconditionally, which would stamp every EDIT-driven closure `retracted_source` as well. See §7
open question 10.

`sets_superseded_by: false` on both closures is deliberate (R2/CM-2): a corrected or retracted
fact was not superseded by a newer value, so the compatibility columns `superseded_by`/
`superseded_at` stay NULL and the compat view keeps meaning what it says. It also means
`is_open_fact()` — not `superseded_by IS NULL` — is the only correct openness test for these
rows, which is exactly the sweep of IN-13/CD-2.

### 2.6 Response shapes (additive only)

```jsonc
// engram edit <handle> --content … --json
{ "id": "<uuid>",                      // legacy, unchanged
  "handle": "mem:<uuid>",              // NEW (W8)
  "resolved_from": "chunk:<uuid>",     // NEW, present only when a chunk handle was given
  "updated": true,                     // unchanged
  "chunks_replaced": 3,                // unchanged
  "facts_invalidated": 2,              // NEW (W4) — count of intervals closed as `corrected`
  "facts_invalidated_ids": ["<uuid>"], // NEW — so an agent can inspect what it just closed
  "reextraction": "pending" }          // NEW (W4) — "replayed" | "pending" | "none"

// engram forget <handle> --confirm --json
{ "id": "<uuid>", "handle": "mem:<uuid>",
  "deleted": true,                     // unchanged: true = this call retracted it
  "already_retracted": false,          // NEW
  "facts_closed": 4,                   // NEW (W5)
  "facts_closed_ids": ["<uuid>"] }     // NEW

// engram remember … --facts-json … --json  (additive)
{ … existing fields …,
  "handle": "mem:<uuid>",
  "facts_source_kind": "user_stated" } // NEW (W6) — what the batch was stamped

// engram export --json
{ "version": 3,                        // was 2
  "schema_version": 10,                // NEW — store schema the slice came from
  "kb": …, "counts": { …, "events": 1234 },
  "events": [ … ],                     // NEW — the journal slice
  "kbs": …, "documents": …, "memories": …, "chunks": …, "facts": … }  // retained, see §3.6

// engram import <file> --json
{ "format_version": 3,
  "replayed": { "events": 1234, "memories": 88, "facts": 240, "decisions": 12, "conflicts": 3 },
  "imported": { "chunks": 300 },       // embeddings, which the journal cannot rebuild
  "skipped_existing": { … },
  "next_step": "run `engram compile --kb <name> --all` to regenerate derived claims/entities/wiki" }
```

`metadata` mirrors `facts_invalidated`, `reextraction`, `facts_closed`, `already_retracted` and
`facts_source_kind`, because half the agent surface reads `metadata` and half reads `data`, and
the redesign's answer to that is "both, always" (interfaces.md §4).

---

## 3. File-by-file change plan

Line numbers are the tree at 0.3.1 + wave-1 pre-landings (the commit this spec was written
against). Each item names the function, not just the line, so a moved line does not orphan it.

### 3.1 `crates/engram-cli/src/commands/memory.rs` (23 lines today → ~140)

| Change | Where |
|---|---|
| **NEW** `pub(crate) struct MemoryTarget` + `pub(crate) fn resolve_memory_target(ctx, raw, command)` per §2.1 | top of file |
| **NEW** `pub(crate) fn render_handle(id: Uuid) -> String` — `Handle::render(HandleKind::Memory, id)`, one place so no surface hand-builds `"mem:"` | top of file |
| `list()` (`memory.rs:8-23`) — each row in `data.memories` gains `handle`. `Memory` is serialized wholesale today (`json!({"memories": memories})`); do NOT add a field to `engram_core::types::Memory` (not this lane's type). Map to `serde_json::Value` and insert `handle` per row. | `list`, body |
| Module doc rewritten: this file is now "the memory surface's shared id resolution", not "simple UX helpers" | `memory.rs:1` |

The helper lives here rather than in `commands/mod.rs` (lead-owned, CC-5) or a new module
(needs a `pub mod` line this lane may not write). Cost: zero cross-lane patches for W1.

### 3.2 `crates/engram-cli/src/commands/edit.rs` (238 lines)

| # | Change | Anchor |
|---|---|---|
| E1 | Replace `Uuid::parse_str(&id)` + `ctx.store.memory_scope(uuid)` with `let target = memory::resolve_memory_target(ctx, &id, "edit")?;` — deleting the `BadInput("no memory found for id {id} (already deleted?)")` branch that made every wrong-kind id look like a race | `run`, `edit.rs:48-64` |
| E2 | Compute `prev_content_hash` from the CURRENT row before the swap (`ctx.store.memory_content(uuid)?` → hash) and `content_hash` from the new content. Both are needed by `memory.revised` and both must be read inside the write transaction to be correct under a concurrent edit; read the previous content inside `with_tx`, hash both there | `run`, new lines before `edit.rs:183` |
| E3 | **Delete `store.delete_facts_for_memory(uuid)?`** (`edit.rs:190`). Replace with:<br>`let invalidated = store.close_facts_for_memory(uuid, now, CloseReason::Corrected, &CloseAuthority::Caller { actor }, &actor)?;`<br>`store.clear_extraction_marker(uuid)?;` — **but see E7a: the marker clear is CONDITIONAL and its condition must be evaluated BEFORE this line, inside the same transaction** | `run`, inside the `with_tx` closure, `edit.rs:183-210` |
| E4 | Append `memory.revised` inside the SAME `with_tx` closure, after `update_memory` succeeds and before the chunk inserts — `EventDraft { kind: MemoryRevised, subject_id: Some(uuid), kb, diary, actor, payload }` | `run`, `edit.rs:184-192` |
| E5 | Append `memory.meta_changed` when `importance.is_some()`, in the same closure. The importance-only branch (`edit.rs:212-220`) gets its own `with_tx` so it, too, is event + effect in one transaction — today it is a bare `update_memory` with no transaction at all | `run`, `edit.rs:212-220` |
| E6 | The `with_tx` closure's return type changes from `usize` to a small struct `EditTxOut { chunks_replaced: usize, facts_invalidated: Vec<Uuid>, prev_hash: String }` — the closure already returns `Ok(replaced)`, this widens it | `edit.rs:183-210` |
| E7 | `reextraction` decision. **The earlier draft made this decision AFTER the transaction while E3 cleared the marker unconditionally INSIDE it, so `"replayed"` could never be honest**: by the time the ledger was consulted the marker was already gone and the facts already closed, and nothing in this lane restores derivation outputs. Corrected control flow in E7a | `run`, inside then after `with_tx` |
| E7a | Inside `with_tx`, before E3's close/clear, and only when `content.is_some()`: `let replayed = store.derivation_for(&new_hash, EDIT_EXTRACTOR_ID)?.is_some();` Then `close_facts_for_memory` runs either way (the OLD content's facts are invalid regardless), but `clear_extraction_marker` runs **only when `!replayed`** — a hit means this exact content has already been derived by this extractor, so the memory is not pending. After the transaction: `importance`-only → `"none"`; `replayed` → `"replayed"`; else → `"pending"`. Until WR-3 lands, `replayed` is hard-coded `false` and the branch is dead but present, so wave 4 turns it on by deleting one line rather than by re-deriving this ordering | `run`, `edit.rs:183-210` |
| E7b | `EDIT_EXTRACTOR_ID` is the `extractor_id@version` string a `facts backfill` over this memory would record (storage-journal §2.4). This lane does not invent it; if fact-semantics/daemon-jobs have not pinned it by integration, `replayed` stays `false` and the constant is `todo!()`-free by being `Option<&str>` = `None`, which short-circuits E7a to `pending` | `run` |
| E8 | `print_success` payload gains `handle`, `resolved_from`, `facts_invalidated`, `facts_invalidated_ids`, `reextraction`; `Metadata` gains the same. Human line becomes `Updated mem:<uuid> (3 chunks, 2 facts closed, re-extraction pending)` | `run`, `edit.rs:227-236` |
| E9 | The `NotFound` inside the transaction (`edit.rs:185-189`) is reachable only by a `forget` racing this edit — not the caller's typo — and must stay **exit 1**. Today that is automatic (`CliError::Storage` → 1, `error.rs:50`); **R11 deletes that mapping**: `StorageError::NotFound → ErrorCode::NotFound → exit 3` (interfaces.md §4). So a bare `?` on that error silently flips a race from transient to bad-input the moment contract-codegen lands. The storage error must therefore be caught and re-wrapped: `.map_err(\|e\| match e { StorageError::NotFound(m) => CliError::Transient(format!("memory {uuid} was retracted while this edit was in flight: {m}")), other => other.into() })`. Comment kept and extended — it now explains a mapping that R11 works against, not one it grants | `edit.rs:179-189` |
| E10 | Module doc: add the paragraph explaining that an edit no longer deletes facts, and why (the adjudication ledger and the decision journal outlive the content they were derived from) | `edit.rs:1-8` |

Unchanged and deliberately so: the empty-content guard (`edit.rs:36-47`), the zero-chunk
invariant (`:82-86`), embedder resolution and budget order (`:88-107`), the embed-before-teardown
ordering (`:109-148`), and both usage-recording calls (`:127-145`, `:153-170`). The lane's rule is
that an `edit` may not become MORE expensive or less atomic than it is today.

`actor` = `format!("cli:{}", target.diary)`.

### 3.3 `crates/engram-cli/src/commands/forget.rs` (25 lines → ~90)

`run(ctx, id, confirm)` becomes:

1. `--confirm` guard first, unchanged (`forget.rs:10-14`) — a destructive command must refuse
   before it resolves anything.
2. `let target = memory::resolve_memory_target(ctx, &id, "forget")?;` — an unknown uuid is now
   `CliError::NotFound`, exit 3 (**W2**, kills forget.rs:17-21's exit-0 lie). A `chunk:` handle
   retracts the chunk's memory, and `resolved_from` says so. `target.retracted` (WR-9) already
   distinguishes "exists, live" from "exists, already retracted"; `forget` does NOT treat a
   missing `memory_scope` as already-retracted, because a missing scope is also what a genuinely
   absent row looks like, and conflating them is the defect W2 exists to kill.
3. `if target.retracted` → skip the transaction entirely: no event, no closure, `retracted = false`,
   `closed = []`, exit 0. Otherwise one `ctx.store.with_tx(|store| { … })`:
   - `let retracted = store.soft_delete_memory(target.memory_id)?;` — which, per
     spec-storage-journal:798, itself closes the memory's open fact intervals and appends
     `memory.retracted`. This lane does **not** append that event and does **not** call
     `close_facts_for_memory` again (§2.2 ownership warning); it reads the closed ids back for the
     envelope. `soft_delete_memory`'s return must therefore widen, or this lane needs a
     `closed_facts_for_memory_since` read — WR-10.
   - if `!retracted`: the row was retracted between resolution and the transaction (a racing
     `forget`) → same shape as the `target.retracted` branch, exit 0. The race collapses onto the
     idempotent answer, which is the correct one.
4. Response: `{ id, handle, resolved_from?, deleted: retracted, already_retracted: !retracted,
   facts_closed: closed.len(), facts_closed_ids: closed }`, exit 0 in both cases.

`soft_delete_memory` runs its own `with_tx`, which is re-entrant (`sqlite.rs:831-849` returns
`f(self)` when a transaction is already open), so the nesting is safe and the whole retraction is
one transaction — required by invariant 1.

Ordering note: facts are closed AFTER the soft delete inside the same transaction, so a
`close_facts_for_memory` that filters on live memories still sees the row it needs; storage-journal
must not scope that query by `memories.deleted = 0`. Stated here because it is the one ordering
dependency between the two lanes on this path (§6 patch WR-1 note).

### 3.4 `crates/engram-cli/src/commands/remember.rs` (767 lines) — W6 only

The `--facts-json` provenance change, and nothing else. `remember` already writes atomically and
already reports its extraction status honestly; the redesign's remaining defect here is that a
caller-asserted triple is indistinguishable from an extractor guess.

| # | Change | Anchor |
|---|---|---|
| R1 | Split the single `extracted: Vec<ExtractedFact>` into `caller_facts` (parsed from `--facts-json`) and `extracted: Vec<ExtractedFact>` (LLM output). Parsing and its `BadInput` message are unchanged | `run`, `remember.rs:197-202` |
| R2 | `FactBatchPlan` gains `source_kind: SourceKind` (`remember.rs:603-616`); `persist_facts` passes `plan.source_kind` into `FactWriteCtx` instead of the hard-coded `SourceKind::AgentExtracted` | `remember.rs:619-639`, esp. `:632` |
| R3 | Main path: when `caller_facts` is non-empty, persist it as its own batch with `source_kind: UserStated, marker: None`; then, if extraction also ran, persist `extracted` as a second batch with `source_kind: AgentExtracted` and the marker. Fold with the existing `FactWriteOutcome::merge` (`fact_write.rs:445`) | `run`, `remember.rs:492-518` |
| R4 | Duplicate-content path: same split, same order, `marker: None` on both (unchanged — dedup never marks) | `run`, `remember.rs:236-264` |
| R5 | Failure routing preserved *in intent*, **rewritten in code — this is the landmine of §3.4**. Today the guard is `if facts_json.is_some() { return Err(facts_json_failure(…)) }` (`remember.rs:509-510`): it keys on the FLAG, not on which batch failed, because today there is only one batch. After R3 splits them, that condition hard-errors an EXTRACTION failure whenever `--facts-json` was also passed, discarding a caller batch that already committed. The guard must key on the batch: caller-batch failure → `facts_json_failure(...)` (hard, `remember.rs:647-663`); extraction-batch failure → `ExtractionStatus::FailedPersist` (soft, reported) **even when `facts_json.is_some()`**. Tests 30 and 31 exist precisely to pin both halves | `run`, `remember.rs:508-517` |
| R5a | The two batches are only reachable together when `--extract-facts` (clap `extract_facts`, field `force_extract_facts`) is passed alongside `--facts-json`: without it, `remember.rs:452` sets `ExtractionStatus::CallerSupplied` and extraction never runs. "Mixed" is therefore the `facts_json.is_some() && force_extract_facts` path and nothing else; test 29 must pass both flags or it silently tests the caller-only path | `remember.rs:450-456` |
| R6 | `ExtractionStatus::CallerSupplied` unchanged (`"skipped_facts_json"`); envelope gains `facts_source_kind` — `"user_stated"` when a caller batch ran, `"agent_extracted"` when only extraction ran, `"mixed"` when both | `remember.rs:64-99`, `:568-584` |
| R7 | Envelope + metadata gain `handle` | `remember.rs:530`, `:571` |

Why two batches rather than one batch with a per-fact source kind: `ExtractedFact` and
`FactBatch` are fact-semantics' types (`fact_write.rs:598`, `engram_graph::facts`), and widening
them is a cross-lane change to a hot struct. Two batches need nothing from anyone, keep
`write_fact_batch`'s "one unit of work" rule intact per batch, keep the marker with the batch that
earned it, and give the caller-asserted half its own atomicity — which is what
`facts_json_failure`'s retry contract already promises. Cost: the second batch dedups against the
first, which is the correct outcome anyway (an extractor re-deriving a triple the caller stated
should dedup, not double-write).

`write_fact_batch`'s precondition ("no transaction open on this connection", `fact_write.rs:786`)
is respected: both calls are top-level and sequential, exactly as the single call is today.

### 3.5 `crates/engram-cli/src/commands/export.rs` (53 lines → ~110)

`run(ctx, format, kb)`:

1. Format validation unchanged (`export.rs:15-19`).
2. `let doc = ctx.store.export_v3(kb.as_deref())?;` — **one call, and it already carries the v2
   sections.** `ExportV3` (spec-storage-journal §2.13) is events PLUS kbs/documents/memories/
   chunks/facts. The earlier draft kept this command's five existing projection reads
   (`export.rs:21-25`) alongside an events-only slice; that duplicates storage's own reads and
   invites the two halves to disagree about `kb` scoping (`export_v3(Some(kb))` deliberately also
   emits `kb IS NULL` subject events, spec-storage-journal:763 — a hand-rolled projection read has
   no equivalent rule). Delete the five reads; serialize `doc`.
3. Why v3 is a superset rather than a replacement, unchanged: the journal cannot rebuild
   embeddings (a replay would have to re-embed, at cost, with a possibly different model), so
   `chunks` with their base64 vectors remain the only lossless carrier of the index.
4. Payload: `version: 3`, `schema_version` from `doc`, `events`, `counts.events`, and every v2
   section unchanged in name and shape — all of which come out of `ExportV3`'s serde form, so
   "unchanged in name and shape" is storage's obligation and a contract test's assertion, not this
   command's hand-assembly.
5. `metadata` gains `events`.

An old v2-only consumer reading a v3 file still finds `memories`/`chunks`/`facts` where it left
them; `engram import` prefers the journal. `--format` keeps its only legal value `json`.

### 3.6 `crates/engram-cli/src/commands/import.rs` (238 lines → ~330)

| # | Change | Anchor |
|---|---|---|
| I1 | Version dispatch gains `3 => import_v3(ctx, &body)?` (`import.rs:47-50`). `_ => import_v1` stays the fallback, so an un-versioned legacy file is still accepted | `run` |
| I2 | **NEW** `fn import_v3(ctx, body) -> Result<(Value, Metadata), CliError>`: `serde_json::from_value::<ExportV3>(body.clone())` (a malformed `ts`, a `schema_version` ahead of the binary, or a missing section is storage's rejection, not this command's guess — see I2a), then ONE call: `ctx.store.import_v3(&doc)?`. **Not** a hand-rolled pipeline. The earlier draft replayed `events` and then looped `import_chunk_row` — which imports neither `kbs` nor `documents`, so every chunk with a `document_id` FK (`sqlite.rs:521-525, :545`) lands orphaned or fails, and any ingested corpus round-trips into a store with no documents. It also left test 40's idempotency resting on an `INSERT OR IGNORE` on event ids that WR-5 never required | new fn after `import_v2` |
| I2a | Date/shape validation is **storage's**, uniformly, so a CLI import and a daemon import reject the same files. This command's only local check is the envelope unwrap it already does; it does not re-run v2's `parse_from_rfc3339` loops over v3 sections. Test 41 asserts the rejection, not who performed it | `import_v3` |
| I3 | `import_v2` stops calling the five row importers directly and calls `store.import_v2_with_genesis(&kbs, &documents, &memories, &chunks, &facts)?` inside its existing `with_tx` — a v2 file has no journal, and a store restored without one cannot be rebuilt or audited (interfaces.md §2 invariant 4). Using storage's combined call rather than a separate `synthesize_genesis_events(None)` matters: genesis must be generated from the SAME rows in the SAME transaction (it shares `migrate_v10::genesis`, spec-storage-journal:780, :941), or genesis and projections disagree. The date-validation loops (`import.rs:69-98`) stay — they are v2's own contract and predate this change | `import_v2`, `import.rs:63-121` |
| I4 | `import_v1` likewise: convert its rows to the export-row types and call `import_v2_with_genesis` inside the same `with_tx`. A v1 file has no `kbs`/`documents`/`facts` sections, so those slices are empty | `import_v1`, `import.rs:190-222` |
| I5 | Module doc gains the v3 paragraph: what replays (semantics, facts, decisions, conflicts) and what does not (embeddings, which ride in `chunks`) | `import.rs:1-14` |
| I6 | `next_step` string unchanged for v1/v2; v3 keeps the `compile` hint and adds nothing about `reindex` (v3 carries vectors) | all three importers |

Envelope-unwrapping (`import.rs:38-43`) and `section()` (`:159-169`) are untouched and serve v3 too.

### 3.7 Dispatch and clap

`lib.rs` (daemon-jobs) and `cli.rs` (contract-codegen) need **no change for W1–W6**: `id: String`
already reaches `edit::run`/`forget::run`, and `MemoryCommand::Update`/`Delete` already delegate
to them (`lib.rs:261-267`), so the memory aliases inherit typed handles for free. The only clap
requests are the two optional export/import flags in §6, and this lane's plan does not depend on
them landing.

---

## 4. Migration and compatibility

### 4.1 Store migration

None owned here. The v9→v10 migration is storage-journal's (interfaces.md §2.1, R5). This lane
has exactly two dependencies on it and states them so a mismatch is caught at integration rather
than in the field:

1. After migration, every pre-existing memory has a synthesized `memory.written` genesis event;
   otherwise `edit`'s `memory.revised` is the first event about a subject that appears from
   nowhere, and `admin rebuild` reconstructs a store with edits but no originals.
2. Facts that the OLD `edit`/`forget` already deleted are gone and cannot be recovered. The
   migration must not attempt to resurrect them. This lane's fix is forward-only, and the
   CHANGELOG must say so plainly.

### 4.2 CLI compatibility

| Surface | v0.3 behaviour | v0.4 behaviour | Break? |
|---|---|---|---|
| `edit <bare-uuid>` valid memory | works | works | no |
| `edit <bare-uuid>` unknown | exit 3 `bad_input` | exit 3 `not_found` | exit code same, `error.code` string changes |
| `edit <chunk-uuid>` | exit 3 "no memory found (already deleted?)" | succeeds, edits the owning memory, `resolved_from` set | behaviour change, and the point of W1 |
| `edit fact:<uuid>` | exit 3 bad_input (uuid parses, memory lookup fails) | exit 3 `wrong_id_kind` naming `fact:<uuid>` | improvement, same exit |
| `forget <unknown-uuid> --confirm` | **exit 0**, `{"deleted": false}` | **exit 3** `not_found` | **the one intentional exit-code change** |
| `forget <known-uuid>` twice | exit 0 `deleted:true`, then exit 0 `deleted:false` | identical, plus `already_retracted:true` | no |
| `forget --confirm` missing | exit 3 bad_input | unchanged | no |
| `edit --content` then `facts list` | facts gone | facts present, closed, `close_reason:"corrected"` | behaviour change, the point of W3 |
| `remember --facts-json` | facts stamped `agent_extracted` | stamped `user_stated` | data change, the point of W6 |
| `export` | `version: 2` | `version: 3`, all v2 sections still present | additive; a v2 reader that pins `version == 2` breaks |
| `import` of a v1/v2/v3 file | v1/v2 work | all three work | no |

The `forget` exit-code change is the only intentional break, it is named in the brief's move-5
defect list (`forget.rs:17-21`), and it must appear in the CHANGELOG under a **BREAKING for
agents** heading with the one-line migration: *scripts that treated exit 0 as "the id was valid"
must now treat exit 3 / `error.code == "not_found"` as "no such memory" — which is what they
already assumed.*

### 4.3 Data compatibility

- No JSON field is renamed or retyped. `deleted`, `updated`, `chunks_replaced`, `imported`,
  `skipped_existing`, `format_version` keep name, type and meaning.
- `facts_invalidated` counts CLOSED intervals, not deleted rows. The word is kept from the
  redesign brief; the doc comment and `agent-info` text must say "closed, not deleted" or the
  field re-teaches the old model.
- Facts closed by `edit`/`forget` have `valid_until` set and `superseded_by` NULL. Any consumer
  still testing `superseded_by IS NULL` for openness sees them as open. That is IN-13/CD-2's
  sweep, owned by the lead's wave-3 integration gate — this lane's tests assert the STORED shape
  (`valid_until` set, `close_reason` correct) and mark listing-level assertions `#[ignore]` with
  the blocking gate named, per CC-4.

### 4.4 Ordering within wave 3

This lane compiles only after storage-journal lands `resolve_handle`, `append_event`,
`close_facts_for_memory`, `clear_extraction_marker`, `memory_scope_including_deleted` and the
non-deleting `soft_delete_memory` (WR-1/2/4/9). It does **not** stub them locally: a stubbed
`close_facts_for_memory` that returns `Ok(vec![])` is a silent regression to hard-delete semantics
with a green test suite. If they are missing, this lane implements W6 (remember, needs nothing) and
W1's parse-side error mapping, and blocks the rest with a written report — the same discipline
spec-fact-semantics §6.8 applies to `binding_decision`.

It is also blocked on a DECISION, not just on code: open question 9 (who appends the events).
Every line of E4, E5 and §3.3 step 3 differs between the two answers, and guessing produces either
duplicate events or none. The lane's first action at wave-3 kickoff is to ask for that ruling.

---

## 5. Test plan

**Isolation, non-negotiable (constraint 1, R14).** Every new test sets `ENGRAM_STORE` to a
`TempDir` and never relies on HOME. New tests live in
`crates/engram-cli/tests/wave3_write_surfaces.rs` (NEW, owned by this lane; integration test files
need no `mod` declaration, so no cross-lane patch). Shared local helper:

```rust
fn store() -> tempfile::TempDir { tempfile::tempdir().expect("tmp store") }

fn engram(store: &std::path::Path) -> assert_cmd::Command {
    let mut c = assert_cmd::Command::cargo_bin("engram").expect("binary built");
    c.env("ENGRAM_STORE", store);            // the ONE isolation mechanism (R14)
    c.env("ENGRAM_BENCH_FORCE_STUB", "1");   // no key, no network, deterministic vectors
    c.env_remove("OPENROUTER_API_KEY");      // extraction must not vary with the dev's shell
    c.env_remove("GEMINI_API_KEY");
    c
}

/// Store a memory offline and return (memory_id, first chunk_id).
fn seed(store: &std::path::Path, content: &str) -> (String, String);
```

`ENGRAM_STORE` roots db/vectors/config/cache (landed in `paths.rs`), so a leaked path fails
visibly rather than touching `~/Library/Application Support/bio.199-biotechnologies.engram/`.

Facts, events and decisions are asserted by opening the temp store directly —
`engram_storage::SqliteStore::open(dir)` is available to integration tests (it is a normal
dependency of this package, `crates/engram-cli/Cargo.toml:24`) — and always AFTER the spawned
binary has exited, so the two processes never contend for the writer lock. Assertions that depend
on a read surface (`facts list` showing/hiding closed rows) belong to read-surfaces and are the
ones marked `#[ignore]` pending the `is_open_fact()` sweep.
A guard test asserts the temp store's `db.sqlite` exists and that `$HOME` has no engram dir after
the suite — cheap, and it catches the exact bug that once bricked the installed binary.

### 5.1 Handle resolution (W1, W8)

| # | Test | Asserts |
|---|---|---|
| 1 | `edit_accepts_a_bare_uuid` | v0.3 form still works; exit 0, `updated:true` |
| 2 | `edit_accepts_a_mem_prefixed_handle` | `mem:<uuid>` works; `data.handle == "mem:<uuid>"` |
| 3 | `edit_accepts_a_chunk_handle_and_edits_its_memory` | recall a chunk id, feed it to `edit`, exit 0, `resolved_from == "chunk:<uuid>"`, the MEMORY's content changed |
| 4 | `edit_rejects_a_fact_handle_with_wrong_id_kind` | exit 3, `error.code == "wrong_id_kind"`, message names `fact:` and the correct `mem:` handle is absent (there is none) |
| 4a | `edit_on_a_mem_prefixed_chunk_uuid_is_wrong_id_kind` | the two-step probe (§2.1 step 2b): `mem:<chunk-uuid>` — first probe misses because storage only looks in `memories` — must be `wrong_id_kind` with `actual == "chunk"` and `correct == "mem:<owning-memory>"`, NOT `not_found`. Without step 2b this test is the one that fails |
| 4b | `edit_on_a_fact_handle_offers_the_owning_memory` | `edit fact:<uuid>` → `wrong_id_kind`, `correct == "mem:<source_memory_id>"`; the agent's next command is in the error |
| 5 | `edit_on_an_unknown_uuid_is_not_found` | exit 3, `error.code == "not_found"`; and `edit mem:<unknown-uuid>` is ALSO `not_found` (both probes miss), not `wrong_id_kind` |
| 6 | `edit_on_a_malformed_id_keeps_the_legacy_message` | exit 3, `bad_input`, message contains `invalid UUID` |
| 7 | `edit_on_an_unknown_uuid_never_calls_the_embedder` | with `GEMINI_API_KEY` set to a poison value and no `--offline`, exit is 3 and no usage row exists — the paid-call guard of edit.rs:51-64 survives the rewrite |
| 8 | `memory_update_alias_resolves_handles_identically` | `memory update chunk:<id>` == `edit chunk:<id>` |
| 9 | `every_write_response_carries_a_mem_handle` | `remember`, `edit`, `forget` all emit `data.handle` matching `^mem:[0-9a-f-]{36}$` |

### 5.2 forget (W2, W5)

| # | Test | Asserts |
|---|---|---|
| 10 | `forget_on_an_unknown_id_is_exit_3_not_found` | **the headline regression test**: exit 3, `error.code=="not_found"`, and stdout is NOT a success envelope |
| 11 | `forget_twice_is_idempotent_and_still_exit_0` | second call exit 0, `deleted:false`, `already_retracted:true` |
| 12 | `forget_keeps_facts_as_closed_intervals` | seed with `--facts-json`, forget, then open the temp store directly and assert the fact row still exists with `valid_until` set and `close_reason == "retracted_source"` |
| 13 | `forget_appends_memory_retracted` | `events` table has exactly one `memory.retracted` for the subject, and one `fact.interval_closed` per closed fact, all with the same transaction's timestamp ordering |
| 14 | `forget_without_confirm_still_refuses_before_resolving` | `forget <unknown> ` with no `--confirm` is `bad_input`, NOT `not_found` — the destructive guard precedes resolution |
| 15 | `forget_accepts_a_chunk_handle` | retracts the owning memory |
| 16 | `a_retracted_memory_is_invisible_to_recall` | unchanged v0.3 behaviour; the soft delete still hides content |

### 5.3 edit (W3, W4)

| # | Test | Asserts |
|---|---|---|
| 17 | `edit_no_longer_deletes_facts` | **the move-1 headline**: seed a memory + facts, `edit --content`, assert every fact id still exists |
| 18 | `edit_closes_old_facts_as_corrected` | closed facts carry `valid_until` and `close_reason == "corrected"`, `superseded_by` NULL |
| 19 | `edit_reports_facts_invalidated` | `data.facts_invalidated == 2`, `facts_invalidated_ids` matches the fact ids |
| 20 | `edit_preserves_the_adjudication_ledger` | seed two conflicting facts so a row lands in `fact_conflict_queue`, resolve it, `edit --content`, assert the `fact_decisions` row and the queue row still exist — the FK cascade (sqlite.rs:1106-1108) is no longer reachable. `fact_decisions` does not exist in the 0.3.1 tree; it arrives with the v10 migration, so the decisions half is `#[ignore]`d until storage-journal lands and the queue half runs from day one. The decisions table's own FK behaviour toward `facts` is storage-journal's to state — this test asserts survival, not the mechanism |
| 21 | `edit_appends_memory_revised_with_both_hashes` | one `memory.revised` whose `prev_content_hash` equals the `memory.written` hash and whose `content_hash` differs |
| 22 | `edit_importance_only_appends_meta_changed_and_touches_no_fact` | `reextraction == "none"`, `facts_invalidated == 0`, one `memory.meta_changed`, zero `fact.interval_closed` |
| 23 | `edit_marks_the_memory_pending_reextraction` | `reextraction == "pending"` and `facts backfill --dry-run` lists the memory — the marker really was cleared (kills the marker-only pending predicate mismatch of sqlite.rs:734-735) |
| 24 | `edit_is_atomic_under_an_injected_failure` | with `ENGRAM_FACT_WRITE_FAIL` / the `injected_embed_failure("edit")` seam, nothing changed: content, chunks, facts and events all as before |
| 25 | `edit_writes_its_event_in_the_same_transaction` | after an injected post-update failure, there is no orphan `memory.revised` (invariant 1) |
| 26 | `edit_to_identical_content_is_still_one_revision` | hashes equal → still appends `memory.revised` (the caller asked), still `facts_invalidated == 0`, `reextraction == "replayed"` once WR-3 lands, `"pending"` before then — asserted as "one of the two", with the strict form `#[ignore]`d and named |

### 5.4 remember / --facts-json (W6)

| # | Test | Asserts |
|---|---|---|
| 27 | `facts_json_is_stamped_user_stated` | `facts list --json` shows `source_kind == "user_stated"` for every caller triple |
| 28 | `extraction_stays_agent_extracted` | offline stub extraction path still stamps `agent_extracted` |
| 29 | `mixed_facts_json_and_extract_facts_keeps_both_provenances` | `--facts-json … --extract-facts`: caller triples `user_stated`, extracted triples `agent_extracted`, `data.facts_source_kind == "mixed"` |
| 30 | `a_failing_caller_batch_still_hard_errors` | `ENGRAM_FACT_WRITE_FAIL` targeting the caller triple → non-zero exit with the `--facts-json:` retry message; the extraction batch's outcome is not reported as if committed |
| 31 | `a_failing_extraction_batch_leaves_the_caller_batch_committed` | the inverse: exit 0, caller facts present, `fact_extraction == "failed_persist"` |
| 32 | `the_duplicate_content_path_uses_the_same_source_kinds` | re-run the identical `remember --facts-json`, dedup path attaches nothing new and stamps nothing differently |
| 33 | `user_stated_facts_reach_the_correction_rule` | with two contradicting single-valued facts where the second is caller-stated, the classifier's `correction_by_user` path is reachable (assert on the resolved/queued shape fact-semantics defines) — `#[ignore]` until fact-semantics lands, blocking lane named |

### 5.5 import / export (W7)

| # | Test | Asserts |
|---|---|---|
| 34 | `export_emits_version_3_with_a_journal_slice` | `version == 3`, `events` non-empty, `counts.events` matches |
| 35 | `export_still_carries_every_v2_section` | `kbs/documents/memories/chunks/facts` present with the same field names — a v2 consumer is not broken |
| 36 | `export_then_import_round_trips_into_an_empty_store` | two TempDirs; after import, `recall` finds the memory WITHOUT a reindex (embeddings survived) and `facts list` shows the same fact ids |
| 36a | `a_v3_round_trip_carries_documents_and_kbs` | ingest a document into store A (so `chunks.document_id` is set), export, import into B: `documents list` and `kb list` both show the row, and no chunk is orphaned. The events-only import this spec originally described passes test 36 and fails this one — which is why it is separate |
| 37 | `round_trip_preserves_closed_intervals_and_decisions` | forget a memory in store A, export, import into B: the fact is present and closed with `retracted_source`, and the decision rows came across |
| 38 | `importing_a_v2_file_synthesizes_genesis_events` | a hand-written v2 fixture → after import the `events` table is non-empty and `admin rebuild` reproduces the projections (the rebuild half `#[ignore]`d until storage-journal's `admin rebuild` lands) |
| 39 | `importing_a_v1_file_still_works_and_still_needs_reindex` | unchanged legacy behaviour + genesis events |
| 40 | `import_is_idempotent` | importing the same v3 file twice adds no duplicate events, memories or facts. This rests entirely on `import_v3` preserving event ids and inserting them idempotently — a storage obligation (WR-5), not something this command can arrange. Named here so a red test is routed to storage-journal rather than debugged in `import.rs` |
| 41 | `a_malformed_slice_is_rejected_not_guessed` | an `events` array with an unparseable `ts` → exit 3 `bad_input`, store unchanged |

### 5.6 Regression tests in files this lane does not own

`crates/engram-cli/tests/cli.rs` is lead-owned (R15). Two existing tests need updating and both
changes go through the patch channel (§6), each with a message saying the assertion WAS the bug:

- `forget_with_bad_uuid_is_bad_input` (`cli.rs:896-899`) — unchanged, still `bad_input`; verified
  only, no edit.
- any test asserting `forget` exit 0 on an unknown id — none exists today (grepped: `cli.rs:888`
  and `:896` cover missing-confirm and bad-uuid only), so W2 breaks no existing test. Stated
  because it is the kind of claim a reviewer must be able to check.

### 5.7 Gates before commit (constraint 5)

`cargo build --workspace`, `cargo clippy --workspace -- -D warnings`, `cargo fmt --check`,
`cargo test --workspace`, and `engram bench personal --json` run with `ENGRAM_STORE` pointed at a
TempDir, output unchanged. The `#[ignore]`d tests (26 strict form, 33, 38's rebuild half) are
listed in this lane's report with their blocking lane, per CC-4's pin.

---

## 6. cross_lane_patches

One-liners this lane needs in files it does not own. Each is stated as the exact edit, the owner,
and what breaks without it. The lead applies them at wave integration (R15).

### 6.1 contract-codegen — `crates/engram-cli/src/error.rs`

| id | Patch | Without it |
|---|---|---|
| **XC-1** | **Already spec'd by contract-codegen §2.3** as `NotFound { what, handle }` and `WrongIdKind { handle, actual, expected, correct }`; this lane codes against THOSE shapes (§2.3). The only request is that they land in wave 3, not later, and that `what` stays `&'static str` | W1/W2 can only be expressed as `BadInput`, so agents cannot branch on the error kind and the typed-handle promise of move 5 is cosmetic |
| **XC-2** | `emit_to_stderr`'s JSON error object surfaces `handle`, `actual`, `expected` and `correct` when the variant carries them | the correct handle is only in prose; an agent has to regex the message |
| **XC-5** | `StorageError::NotFound` → exit 3 (R11) must not silently reclassify `edit`'s in-transaction race. Contract-codegen keeps a `CliError::Transient(String)` constructor public so E9 can re-wrap | a retryable race is reported to agents as bad input, and retry loops stop retrying |
| **XC-3** | `agent-info` / `skill.md` text: document `facts_invalidated` as *facts closed, not deleted*, `reextraction: replayed\|pending\|none`, `already_retracted`, `facts_closed`, `handle`, `resolved_from`, `facts_source_kind`, and `export version 3`. The generated contract test fails on an undocumented field (CD-10), so this lands in the SAME wave as W3–W7 | red contract test in a lane that did nothing wrong |
| **XC-4** | (optional, deferred) `cli.rs`: `export --compat-v2` and `import --dry-run`. Not required by any deliverable; requested only if the lead wants a downgrade path pre-announced | nothing breaks; §3.5's superset design already covers v2 readers |

### 6.2 storage-journal — `crates/engram-storage/src/sqlite.rs`

| id | Patch | Without it |
|---|---|---|
| **WR-1** | `close_facts_for_memory(memory_id, at, reason, authority, actor) -> Result<Vec<Uuid>, StorageError>`, appending one `fact.interval_closed` per fact in the caller's transaction. **Must not filter on `memories.deleted = 0`** — `forget` calls it after the soft delete, in the same transaction (§3.3) | `edit`/`forget` have no non-destructive way to invalidate facts; W3/W5 cannot be built |
| **WR-2** | `clear_extraction_marker(memory_id) -> Result<bool, StorageError>` — split out of `delete_facts_for_memory` (sqlite.rs:1925-1935) | `edit` must choose between deleting facts and leaving the memory permanently marked as extracted; the re-extraction time bomb survives |
| **WR-3** | `derivation_for(content_hash, extractor_id) -> Result<Option<DerivationRow>, StorageError>` — already contracted at spec-storage-journal §2.4; this lane needs it EXPORTED and needs the `extractor_id@version` string `facts backfill` records (may be wave 4) | `reextraction` can never report `"replayed"`; the value stays contracted-but-unreachable, and test 26's strict form stays `#[ignore]`d |
| **WR-4** | `soft_delete_memory` drops its `DELETE FROM facts WHERE source_memory_id = ?1` (sqlite.rs:1862-1865); keeps `delete_derived_for_memory_chunks` | `forget` hard-deletes facts before this lane can close them; W5 is unbuildable, and the brief's move-1 defect at sqlite.rs:1855-1869 stands |
| **WR-5** | `export_v3(kb)`, `import_v3(&doc)`, `import_v2_with_genesis(...)` + `ExportV3` / `ImportV3Report`, `serde`-round-trippable, `pub` from `engram_storage` — all already contracted at spec-storage-journal §2.13. This lane only needs them exported and needs `ImportV3Report`'s counter names frozen for the envelope | W7 has no format to wire; CD-9's ownership split has no seam |
| **WR-6** | `pub fn content_hash(s: &str) -> String` at `engram_core::keys` (spec-storage-journal:164, :777), re-exported or path-imported, the SAME function `derivation.ran` uses | `memory.revised` and the derivation ledger can disagree about what "same content" means, and `reextraction: "replayed"` silently never fires |
| **WR-7** | `memory_content(memory_id) -> Result<Option<String>, StorageError>` (or make `prev_content_hash` an out-param of `update_memory`). **Moot if Q9 rules that `update_memory` appends `memory.revised` itself** — it already has both contents in hand at `:1897-1919` and needs no help from the CLI | `edit` cannot compute `prev_content_hash` inside the transaction, so the revision chain is unverifiable |
| **WR-8** | `resolve_handle` must return `Some` for SOFT-DELETED memories (kind `Memory`), not `None` — already granted at spec-storage-journal §2.7 ("Soft-deleted memories resolve") | `forget` cannot distinguish "unknown id" (exit 3) from "already retracted" (exit 0), and W2 becomes a false `not_found` on every double-forget |
| **WR-9** | `memory_scope_including_deleted(id) -> Result<Option<(String, String, bool)>, StorageError>` — `(kb, diary, deleted)`. `memory_scope` filters `deleted = 0` (`sqlite.rs:2763`), and `Resolved` carries no `deleted` flag by design (spec-storage-journal §2.7 says "callers that care read the memory row") | `resolve_memory_target` cannot build a `MemoryTarget` for an already-retracted memory: no `kb`, no `diary`, therefore no `actor`. W2's exit-0 half is unimplementable and the implementer invents a query |
| **WR-10** | `soft_delete_memory` returns the ids it closed — e.g. `-> Result<Option<RetractOutcome { closed: Vec<Uuid> }>, StorageError>` — OR keeps `bool` and this lane reads them back. Needed because storage, not this lane, performs the closure (§2.2) | `facts_closed` / `facts_closed_ids` (W5) can only be produced by a second query that races nothing but duplicates storage's work |

### 6.3 fact-semantics — `crates/engram-cli/src/fact_write.rs`

| id | Patch | Without it |
|---|---|---|
| **XF-1** | None required. `FactWriteCtx.source_kind` already exists (`fact_write.rs:598`) and `FactWriteOutcome::merge` already exists (`:445`); W6 is implemented entirely inside `remember.rs`. Listed explicitly so the lead does not schedule a patch that is not needed | — |
| **XF-2** | *Advisory:* the classifier's `correction_by_user` rule (spec-fact-semantics §2.3.1 step 7) becomes live the moment W6 lands. Fact-semantics should assume `UserStated` facts now occur in production and keep test 21 as a real path, not a synthetic one | a rule that has never run against real input ships unexercised |

### 6.4 lead — `crates/engram-cli/tests/cli.rs`

| id | Patch | Without it |
|---|---|---|
| **XL-1** | `point_at_home` additionally sets `ENGRAM_STORE` to the same TempDir (already scheduled as a wave-2 lead patch, R14) | legacy tests and new tests isolate by different mechanisms; a `--store`-era regression hides |
| **XL-2** | CHANGELOG entry under **BREAKING for agents**: `forget` on an unknown id is now exit 3 `not_found` (was exit 0 `deleted:false`); `edit` no longer deletes facts; `export` is version 3 | the one intentional break ships undocumented |

---

## 7. Risks and open questions

1. **`reextraction: "replayed"` has no producer in wave 3.** The value is contracted in
   interfaces.md §4 and enumerated by contract-codegen, but the content-addressed derivation
   ledger (move 3) is storage-journal + daemon-jobs work. This lane emits `"pending"` or `"none"`
   until WR-3 lands. *Open question for the lead:* is a contracted-but-unreachable enum value
   acceptable for one wave, or should `"replayed"` be omitted from the generated schema until it
   can fire? This spec assumes the former (agents can already handle it; removing it later would
   be the real break).
2. **Two batches change the adjudication order for mixed `--facts-json --extract-facts` runs.**
   Caller triples are adjudicated first and are in the store when the extraction batch is
   classified, so an extracted triple that contradicts a caller triple now meets a `UserStated`
   incumbent instead of racing it inside one batch. That is the intended direction (the caller's
   assertion is the stronger claim), but it is a behaviour change in a path fact-semantics'
   classifier owns. *Flagged for fact-semantics review.*
3. **`edit` closing every fact from the old content is coarse.** Move 3 promises "re-derive only
   changed spans"; wave 3 closes all of the memory's open facts and marks the memory pending.
   A memory whose edit fixed a typo therefore closes and re-observes facts that never changed,
   producing churn in the journal and new fact ids for unchanged truths. Mitigations available
   later: span-level derivation (move 3 proper), or a re-observation that reuses the fact id when
   the derived triple is identical. *Open question:* should wave 3 skip closing facts whose
   normalized triple survives the re-derivation? This spec says no — the re-derivation has not
   run yet at close time, and guessing is worse than churn.
4. **Export size roughly doubles.** v3 carries both the journal and the projections. Content is
   duplicated (events hold `content`, `memories` holds it too); embeddings are not. For a store
   of a few thousand memories this is tens of megabytes, which is acceptable for a backup format
   and unacceptable for a sync protocol. If export ever becomes a sync path, the projections
   should be droppable behind a flag — noted, not built.
5. **`close_facts_for_memory` writes one event per fact.** A memory with 40 facts produces 41
   events per edit. The journal is append-only and never pruned, so a heavily-edited store grows
   linearly in edits × facts. Measure at the wave-6 gate; a batched `fact.intervals_closed`
   payload is the obvious remedy and is a storage-journal schema change, not this lane's.
6. **Chunks are still hard-deleted on edit** (`delete_chunks_for_memory`, edit.rs:192). Defended
   as index, not history — `memory.revised` carries the content, so a rebuild can re-chunk. But a
   rebuild cannot re-EMBED without paying a provider, so `admin rebuild` cannot restore vectors
   for edited memories from the journal alone. *Open question for storage-journal:* does
   `admin rebuild` preserve the existing `chunks` table (rebuild semantics only), or does
   invariant 3's "byte-identical projection state" include embeddings? This lane assumes chunks
   and their vectors are preserved, not rebuilt.
7. **`forget` on a chunk handle retracts the whole memory.** Correct (a chunk is not a unit of
   retraction) but surprising. The response's `resolved_from` field is the whole mitigation; if
   the lead wants a confirmation prompt for that case, it is one condition in `forget.rs`.
8. **Nothing here has been run against a real store, by constraint.** Every line number is from
   the 0.3.1 tree, and the behavioural claims about `soft_delete_memory` and
   `delete_facts_for_memory` are read from source (sqlite.rs:1855-1869, :1925-1935), not observed.

### Raised by the adversarial review (`docs/arch/review-write-surfaces.md`)

9. **Who appends the memory and fact events — storage or the CLI? LEAD RULING REQUIRED, and this
   lane cannot start E4/E5/§3.3 without it.** `spec-storage-journal.md` §5 puts `memory.revised`
   and `memory.meta_changed` inside `update_memory` (`:1897-1919`), and `memory.retracted` plus a
   per-fact `close_fact_interval` inside `soft_delete_memory` (`:1855-1870`). This spec's E4/E5 and
   §3.3 originally appended the same events from the CLI. Both landing as written = duplicate
   events and double-closed intervals; either alone = the other lane's plan is fiction.
   `interfaces.md` §2 invariant 1 says only that the append shares the effect's transaction — it
   never assigns the writer. §2.2 and §2.5 now assume **storage appends**, because storage owns
   both the row and the transaction and because a daemon or HTTP write path must produce the same
   journal as the CLI; if the lead rules the other way, §2.2's warning, §2.5's table and §3.3's
   step 3 all invert together and WR-10 disappears.
10. **`close_reason` and `authority` for an edit-driven closure.** This spec wants
    `reason="corrected", authority="caller:<actor>"` for `edit` and `reason="retracted_source"`
    for `forget`. `spec-storage-journal.md` :798 has `soft_delete_memory` close with
    `authority="explicit_end"` (not `caller` — and a user `forget` is not an explicit end date),
    and :800 rewrites `delete_facts_for_memory` to close with `retracted_source` unconditionally
    while calling the change "a rename, not a redesign" — which would stamp every `edit`-closed
    interval `retracted_source` and erase the corrected/retracted distinction W3 and W5 exist to
    draw. Additionally the wire form of `authority` is unpinned: interfaces.md §2 has
    `CloseAuthority::Caller { actor }`, storage's examples use bare `"explicit_end"` / `"caller"`,
    and this spec invented `"caller:<actor>"`. R2/CM-2 requires `close_authority` to be rebuildable
    from the event, so ONE canonical string is mandatory. *Proposed:* `corrected`/`caller` for
    edit, `retracted_source`/`caller` for forget, actor carried in the event's own `actor` column
    where it already lives.
11. **`export`'s `version` field changes value 2 → 3.** Constraint 3 says existing fields keep
    name and type; it is silent on values, and §4.2 already books this as a break for a consumer
    that pins `== 2`. XC-4's `--compat-v2` downgrade flag is the mitigation and is currently
    deferred. *Open question for the lead:* is the superset argument (every v2 section still
    present under its old name) enough, or does the discriminator bump need the flag shipped in
    the same wave as the bump?
12. **`delete_document` still hard-deletes facts, and no lane owns it.** `sqlite.rs:2535-2545`
    loops `delete_facts_for_memory` over every memory in a document, then soft-deletes those
    memories, appending no event. It is reachable from `documents delete` (`documents.rs:49`), the
    HTTP surface (`serve.rs:263`) and re-ingest (`ingest.rs:289`). W5 fixes `forget` while this
    path keeps erasing the same rows, and invariant 2 ("nothing is hard-deleted") is false for any
    store that ingests documents. It is outside this lane's owned files. *Flagged for the lead:*
    assign it, or accept that the move-1 headline is half-shipped in v0.4.
13. **`hard_delete_memory` (`sqlite.rs:2085`) also routes through `delete_facts_for_memory`.**
    Storage-journal's :800 rewrite turns that into a fact CLOSURE under a memory row that is about
    to be `DELETE`d, leaving closed intervals pointing at a `source_memory_id` that no longer
    exists. It is an ingest-rollback path (orphan cleanup), so closing is wrong there and deleting
    is right. *Flagged for storage-journal:* the deprecation of `delete_facts_for_memory` needs a
    surviving hard-delete for this one caller.
14. **No signal that pre-v10 facts were erased.** §4.1.2 is forward-only and correct, but an agent
    reading a migrated store sees facts with no closed-interval history and cannot tell "never had
    any" from "the old `edit` deleted them". A `doctor --integrity` note, or a migration-report
    counter, would say so once. Not built; noted so it is a decision rather than an omission.

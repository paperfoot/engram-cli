# spec-read-surfaces.md — implementation spec for the read-surfaces lane (wave 3)

Binding inputs, in precedence order: `docs/arch/interfaces.md` **v2** (rulings R1–R15 override
everything below them), `docs/arch/redesign-brief.md`, `docs/arch/coherence-report.md`,
`docs/arch/spec-fact-semantics.md` (as reviewed; §§6.2–6.4 are the shared boundary).
Where this document and interfaces.md v2 disagree, interfaces.md wins and the divergence is a
bug in this document.

Lane owns, exclusively:

```
crates/engram-cli/src/commands/recall.rs
crates/engram-cli/src/commands/research.rs
crates/engram-cli/src/commands/facts.rs
crates/engram-cli/src/commands/people.rs
crates/engram-cli/src/commands/entities.rs
crates/engram-cli/src/commands/graph.rs
crates/engram-cli/src/commands/wiki.rs
crates/engram-cli/src/commands/documents.rs
crates/engram-cli/src/retrieval.rs
crates/engram-cli/tests/wave3_read_*.rs           (NEW test files, this lane's own)
```

Lane does NOT own and must not edit: `lib.rs` (dispatch — daemon-jobs), `cli.rs` / `output.rs` /
`error.rs` / `agent_info.rs` / `respond.rs` (contract-codegen), `fact_write.rs` / `fact_llm.rs`
(fact-semantics), `serve.rs` (daemon-jobs), anything in `engram-storage` or `engram-core`
(storage-journal). Everything needed in those files is listed in §6 `cross_lane_patches`
(§6.1 one-line patches, §6.2 storage API requests, §6.3–§6.4 requests to sibling lanes).

## 1. Scope

Move 5's read half, plus the fact-read consequences of move 2. Eight deliverables:

1. **Provenance on every hit.** Every result row from `recall`, `research`, `memory search`,
   `people show` snippets and `facts *` carries `memory_id` and a `provenance` chain
   (`chunk → memory → document`), plus a kind-prefixed `handle`. `id` keeps its current meaning
   and type (brief constraint 3) — this is purely additive.
2. **`returned` / `total` / `cursor` on every listing**, with legacy `count` kept as an alias of
   `returned`, and `total` meaning *matching rows in scope*, never *rows on this page*. This is
   the fix for "agents reported 3 conflicts when 953 were open".
3. **`verdict: "results" | "no_match"` plus an absence certificate** (`data.absence`) on every
   search-shaped read, and `metadata.scope { kbs, diary, chunks_considered }` on every read.
4. **`conflicts` array driven by the interval model**, with `suggested_action` per **R7**:
   vocabulary stays `keep-both | supersede-old | requires_context`; safety pairs are ALWAYS
   `requires_context`.
5. **`kb` AND `diary` threaded through every fact and conflict query.** Today `recall`'s conflict
   surface drops `diary` entirely (`recall.rs:305,336`) and `facts conflicts` (history view)
   drops both (`facts.rs:212`).
6. **`memory search` reconciled with `recall`** — same code path, same envelope, same flags, no
   silently different defaults (today `lib.rs:232-257` hard-codes `layer="topic"`,
   `rerank_top_n=None`, `graph_hops=1`, `allow_mixed_embeddings=false`).
7. **The `is_open_fact()` sweep sites this lane owns**: `facts.rs:123`, `facts.rs:156`,
   `facts.rs:157`, `recall.rs:310`. The fifteen `sqlite.rs` sites are storage-journal's; the
   sweep is a lead-owned wave-3 integration gate [CD-2, CC-4].
8. **`LegacyArity` rename (R3)** at `facts.rs:20-24` and `facts.rs:436-439`, and **`respond.rs`
   (R12)** for every `resolve_command` / `resolve {}` payload this lane emits
   (`facts.rs:371-374`, `recall.rs:345-348`).

### 1.1 Non-goals

- The write path, the classifier, `safety_gate`, `SemanticKey`, `WritePlan`, replay — all
  fact-semantics. This lane *calls* them and *renders* their output.
- The journal, `append_event`, `record_decision`, migration v9→v10, `admin rebuild`, export/import
  slice format — storage-journal.
- `respond.rs` itself, `ErrorCode`, `agent-info`, `skill.md`, clap definitions, the envelope
  printer in `output.rs` — contract-codegen. This lane consumes them.
- `serve.rs` / `/v1/recall` — daemon-jobs, wave 4. This lane makes the CLI response builder the
  one implementation so wave 4 has something to call; it does not edit `serve.rs`.
- `engram state` — reasoning-layer, wave 5.
- **Calibrated confidence (brief move 5's second half).** Interfaces v2 §4 dropped `confidence`
  from the pinned envelope and reasoning-layer redefines it for `engram state` in wave 5. This lane
  ships the absence certificate (`verdict` + `absence`) and NOT a confidence score. Do not invent a
  third formula; if the lead wants one on reads, it is a wave-5 request against reasoning-layer's
  definition.
- Retrieval *quality*. Ranking, fusion, rerank, RRF, the diversity cap: untouched. `bench personal`
  must be byte-identical on results ordering; only additive envelope fields change.
- Frozen files (interfaces.md §7): `chunks.rs`, `ingest.rs`, `kb.rs`, `doctor.rs`, `repair.rs`,
  `reindex.rs`, `update.rs`, `usage.rs`, `config.rs`, `budget.rs`, `bench.rs`.

### 1.2 The defects this lane closes (brief anchors, cite in commits)

| Anchor | Defect | Deliverable |
|---|---|---|
| brief move 5 | recall returns chunk ids that `edit`/`forget` reject | 1 |
| brief move 5 | count-means-page: 3 conflicts reported, 953 open | 2 |
| brief move 5 | no absence certificate; empty read is indistinguishable from a scoping mistake | 3 |
| `fact_write.rs:1209-1213` | `suggested_action: "supersede-old"` on safety pairs | 4 |
| `recall.rs:305,336` | conflicts ignore `diary` — cross-diary leak into the conflicts array | 5 |
| `lib.rs:232-257` | `memory search` and `recall` are two commands with one name | 6 |
| `facts.rs:123,156,157`, `recall.rs:310` | a stopped medication reads as current | 7 |
| CM-4 | two `Arity` types, one name | 8 |
| CM-9 / R12 | four lanes ship four `resolve_command` strings | 8 |

### 1.3 A note on `memory.rs` — NOT this lane's

The brief's ownership table gives `crates/engram-cli/src/commands/memory.rs` to **write-surfaces**,
and spec-write-surfaces §3.1 already rewrites that file as the shared handle-resolution home and
adds `handle` to `memory list`. This lane therefore does **not** edit it. `memory list`'s paging
(deliverable 2) is a cross-lane request to write-surfaces, **WS-1** (§6.5); §3.9 states the shape
write-surfaces should implement, nothing more. Two exclusive owners of one 23-line file is exactly
the collision R15's patch channel exists to prevent.

---

## 2. Data model and API changes

**The wire types are contract-codegen's, not this lane's.** `spec-contract-codegen.md` §2.7 ships
`output::{Listing, Scope, Verdict, Absence}` in wave 2 precisely so wave 3 has one spelling to
call, and §7 of that spec names read-surfaces as the caller. This lane therefore **does not declare
`Page` / `ReadScope` / `ReadVerdict` / `Absence`**; declaring a parallel set would recreate the
count-drift defect one layer up and fail `contract_envelope`.

What `crates/engram-cli/src/read_surface.rs` (NEW, this lane's, ~180 lines) actually holds: the
things `output.rs` deliberately does not know about — the cursor codec and its request
fingerprint, `Provenance`, the R7 `suggested_action` guard, and the adapters that turn
`RecallStats` into an `output::Verdict`. The `pub mod read_surface;` line in `lib.rs` is
`cross_lane_patch` CLP-1 (§6).

Why a module and not helpers-per-command: eight commands currently hand-roll the same three
things (page counts, kb/diary scope labels, provenance JSON) in eight slightly different ways, and
that drift is exactly what produced "count means page" on one surface and "count means total" on
another. One builder, one meaning.

### 2.1 Paging — `returned` / `total` / `cursor`

The counters are `output::Listing` (contract-codegen §2.7): `Listing::page(returned, total, cursor)`
writes `returned`, `total`, `count` (= `returned`) and `cursor`-when-`Some`. `total` is the number
of rows matching the request's FILTERS, in the request's SCOPE, ignoring `limit`/`offset`; it is
never `returned`. A caller that sees `returned: 20, total: 953` knows there is a backlog; a caller
that sees `count: 20` (the legacy field, which stays) learns nothing, and did not.

Two deltas this lane needs on `Listing`, requested as **CC-R4** (§6.4):
`Listing::total` typed to carry a SQL `COUNT(*)` (`i64`, or a documented `usize` cast), and a
constructor that takes the cursor from this lane's codec.

The cursor itself is this lane's, because it is a read-surface concern with no envelope meaning:

```rust
// crates/engram-cli/src/read_surface.rs

/// The request fingerprint a cursor is bound to. A cursor issued for
/// `--kb work --diary default` must not silently page a different scope: an
/// offset into a different result set is a wrong answer that looks like a right
/// one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorKey {
    pub surface: &'static str,      // "facts.list", "facts.conflicts", ...
    pub fingerprint: String,        // stable hash of the effective filters
}

impl CursorKey {
    pub fn new(surface: &'static str, parts: &[(&str, Option<&str>)]) -> CursorKey;
}

/// Opaque, URL-safe, versioned: `"1." + base64url(json)` where json is
/// `{"s":<surface>,"f":<fingerprint>,"o":<offset>}`. Opaque by contract — the
/// encoding may change; a caller that parses it is doing so at its own risk and
/// `agent-info` says so.
pub fn encode_cursor(key: &CursorKey, offset: usize) -> String;

/// `BadInput` on a malformed cursor; `BadInput` naming the mismatch when the
/// cursor's surface/fingerprint differs from this request's.
pub fn decode_cursor(raw: &str, key: &CursorKey) -> Result<usize, CliError>;

/// The single cursor-emission rule, so no surface invents its own:
/// `Some(encode_cursor(key, offset + returned))` when `offset + returned < total`,
/// else `None`. An absent cursor is the ONLY end-of-listing signal; callers must
/// not infer the end from `returned < limit` (a filtered page can be short and
/// still have successors).
pub fn next_cursor(key: &CursorKey, offset: usize, returned: usize, total: i64)
    -> Option<String>;
```

`base64 = "0.22"` is already a workspace dependency (`Cargo.toml:49`, used by `engram-storage`), so
`crates/engram-cli/Cargo.toml` adding `base64.workspace = true` is a workspace-dep reuse, not a new
dependency. That one line is CLP-9 if the lead treats manifest edits as cross-lane; otherwise this
lane makes it (the manifest is not in any lane's frozen list).

### 2.2 Scope — `metadata.scope`

The type is `output::Scope { kbs, diary, chunks_considered }` (contract-codegen §2.7), attached via
`Scope::attach(&mut metadata)`. Emitted on EVERY read surface, results or not: an empty result with
`kbs: ["default"]` when the caller expected `["work"]` is a scoping mistake, and today the caller
cannot tell it from an empty store.

How this lane fills it:

- `kbs` — KBs actually searched. Every-KB reads render the resolved list when it is cheap to
  enumerate, else `["*"]`.
- `diary` — `"*"` when every diary was searched.
- `chunks_considered` — `RecallStats::chunks_considered`. **Non-retrieval surfaces (`facts *`,
  `people`, `entities`, `graph`, `wiki`, `documents`) do not score chunks**, and reporting `0`
  there is a lie of the same class as a fabricated threshold. Requested as **CC-R5** (§6.4):
  `Scope::chunks_considered: Option<usize>`, key omitted when `None`. Until CC-R5 lands, those
  surfaces emit `Scope` without the field via a `Scope::listing(kbs, diary)` constructor.
- `origin` — an additive `metadata.scope.origin`, whose vocabulary is **exactly**
  `KbScopeOrigin::as_str()` at `retrieval.rs:460-467`: `"flag" | "config" | "all_kbs"`.
  There is no `config_default`; do not introduce a second vocabulary. `origin` is this lane's
  addition to the scope object and is listed in §4.1.

Non-retrieval surfaces additionally emit `scope.diary_applicable: false` where no diary column
exists (§3.6), rather than pretending a filter ran.

### 2.3 Verdict and the absence certificate

The types are contract-codegen's `output::Verdict { Results, NoMatch(Absence) }` and
`output::Absence { top_score, threshold, searched_kbs, searched_chunks }` (§2.7), attached with
`Verdict::attach`. `data.absence` is present ONLY on `NoMatch`. It is the absence certificate:
proof that the store looked, how hard, and how close it got. Without it "no results" and "you
searched the wrong namespace" are the same response.

This lane requests two deltas, **CC-R6** (§6.4):

- `Absence::threshold: Option<f32>` rather than `f32` — see §2.3.1. As pinned, `threshold` is
  non-optional and this lane has nothing honest to put in it.
- `Absence::reason: &'static str` added, per the decision procedure in §2.3.2 — the field that
  distinguishes an empty store from a wrong namespace.

`read_surface::absence_from(stats, scope) -> output::Absence` is the one place either is built.

#### 2.3.1 The honesty rule for `threshold`

`hybrid_recall` has **no score cutoff today** — it truncates at `top_k` and at
`rerank_top_n`, and returns early with an empty `Vec` when `runs.is_empty()`
(`retrieval.rs:920-924`) or `rerank_candidates.is_empty()` (`retrieval.rs:948-953`). Therefore:

- `threshold` is `None` unless a real cutoff rejected a real candidate. Emitting a fabricated
  `0.0` threshold would be exactly the class of dishonest health signal the 0.3.1 release fixed.
  **If the lead rules that CC-R6 is refused and `threshold` stays `f32`, this lane does not ship
  `absence.threshold` at all rather than ship a zero** — the field is then omitted and OQ-7 becomes
  a wave-4 item behind a real cutoff.
- `no_match` for a retrieval surface means `results.is_empty()` after the layer-budget trim, i.e.
  the existing `status == "no_results"` condition. `verdict` and `status` therefore agree by
  construction; `status` keeps its existing values (`success` / `no_results`) so nothing breaks.
- `top_score` is populated from the fused-candidate pool BEFORE truncation, which requires one new
  field on `RecallOutcome` (§2.5). When the pool was empty, `top_score` is `None`.

#### 2.3.2 `absence.reason` — the decision procedure

`hybrid_recall` does not tag why a pool emptied, so the reason is derived from `RecallStats` and
the request, in this order, first match wins. No other value may be emitted:

| order | condition | `reason` |
|---|---|---|
| 1 | `stats.chunks_considered == 0` and the request carried `--since`/`--until` | `filtered_by_window` |
| 2 | `stats.chunks_considered == 0` and the request carried a non-`*` `--diary` | `filtered_by_diary` |
| 3 | `stats.chunks_considered == 0` | `no_chunks_in_scope` |
| 4 | `stats.chunks_considered > 0` and `stats.candidates_fused == 0` | `no_candidate_matched` |
| 5 | `stats.candidates_fused > 0` (the pool was non-empty but nothing survived the layer-budget trim at `recall.rs:99-111`) | `trimmed_by_layer_budget` |

`no_candidate_above_cutoff` is **deleted from the vocabulary**: there is no cutoff, so the value
could only ever be a lie. Rows 4 and 5 replace it honestly. Row 5 also pins the one place `verdict`
and `stats` could disagree — a non-empty `outcome.results` that renders an empty `results_json` —
by naming it rather than leaving the implementer to discover it.

### 2.4 Provenance chain

```rust
/// chunk -> memory -> document, as far as it is knowable for this hit kind.
/// Every field is Option because a hit kind may genuinely lack a link (a wiki
/// page has no single memory), and a fabricated link is worse than a missing one.
#[derive(Debug, Clone, Default)]
pub struct Provenance {
    pub chunk_id: Option<Uuid>,
    pub memory_id: Option<Uuid>,
    pub document_id: Option<Uuid>,
    pub kb: String,
    pub diary: Option<String>,
    /// Why this row lacks a memory_id, when it does. e.g. "derived:wiki_page",
    /// "derived:entity_aggregate", "derived:relation_edge".
    pub derived_from: Option<&'static str>,
}

impl Provenance {
    /// `{ "chunk_id":…, "memory_id":…, "document_id":…, "kb":…, "diary":…,
    ///    "derived_from":… }`, nulls preserved (agents branch on null; omitting
    ///    the key makes "unknown" indistinguishable from "field not implemented").
    pub fn to_json(&self) -> serde_json::Value;

    /// Kind-prefixed handle for the row's own identity, per interfaces.md §1.
    pub fn handle(kind: HandleKind, id: Uuid) -> String;   // "chunk:<uuid>" etc.
}
```

Per-kind resolution rules (`kind` is `HybridResult::kind`, values today:
`chunk` | `claim` | `wiki` | `entity` | `relation`):

| kind | chunk_id | memory_id | document_id | derived_from |
|---|---|---|---|---|
| `chunk` | the hit id | `StoredChunkEmbedding::memory_id` (already in the in-memory index, `sqlite.rs:100`) | first citation's `document_id` | — |
| `claim` | `CandidateRecord::parent_chunk` (`retrieval.rs:626-628`) | memory of that chunk | citation's `document_id` | — |
| `wiki` | null | null | null | `"derived:wiki_page"` |
| `entity` | null | null | null | `"derived:entity_aggregate"` |
| `relation` | null | null | null | `"derived:relation_edge"` |

No new DB round-trip for `chunk`: `memory_id` is already loaded into `chunk_index`
(`retrieval.rs:~757`). For `claim`, the parent chunk is already carried on `CandidateRecord`; its
memory id comes from the same `chunk_index` when the parent survived the scope filters, else from
the batch lookup requested in §6.2 (SR-1) — one query per response, not per row.

### 2.5 `retrieval.rs` type changes (this lane owns the file)

```rust
pub struct HybridResult {
    pub id: Uuid,
    pub kind: String,
    pub content: String,
    pub score: f32,
    pub rerank_score: Option<f32>,
    pub kb: String,
    pub citations: Vec<SourceCitation>,
    pub sources: Vec<RetrievalSource>,
    /// NEW — resolved during materialization, never re-derived by callers.
    pub provenance: Provenance,
}

pub struct RecallOutcome {
    pub results: Vec<HybridResult>,
    pub notes: RecallNotes,
    /// NEW — populated even when `results` is empty. This is what makes the
    /// absence certificate provable rather than asserted.
    pub stats: RecallStats,
}

#[derive(Debug, Clone, Default)]
pub struct RecallStats {
    /// Chunks the dense pass actually scored (post diary/kb/window filtering).
    pub chunks_considered: usize,
    /// Candidates that entered fusion, all sources.
    pub candidates_fused: usize,
    /// Best fused score before any truncation. `None` when the pool was empty.
    pub top_score: Option<f32>,
    /// KBs represented in the candidate pool — the honest `searched_kbs`.
    pub kbs_searched: Vec<String>,
}
```

`RecallOutcome` is constructed at exactly three sites in `hybrid_recall`, all of which must set
`stats` — miss one and an empty read emits an empty certificate, which is the dishonest-signal
class this deliverable exists to close:

| site | condition |
|---|---|
| `retrieval.rs:920-924` | `runs.is_empty()` — no retrieval run produced a candidate |
| `retrieval.rs:948-953` | `rerank_candidates.is_empty()` — fusion produced ids with no resolvable content |
| `retrieval.rs:1064` (final `Ok`) | the normal path |

Deriving `Default` on `RecallStats` keeps the early-return sites to one line each. A grep for
`Ok(RecallOutcome` must return three hits and no more.

### 2.6 Fact and conflict JSON

`fact_to_json` (`facts.rs:1094-1115`) gains, all additive:

```jsonc
{
  "memory_id": "<uuid>",            // == source_memory_id, named per interfaces.md §1
  "handle": "fact:<uuid>",
  "provenance": { "memory_id": …, "chunk_id": null, "document_id": …, "kb": …, "diary": … },
  "open": true,                     // is_open_fact(f) — the sweep, deliverable 7
  "valid_until": …,                 // already present, now MEANINGFUL
  "closed_by": "<uuid>|null",       // engram-core Fact additions (interfaces.md §3.3)
  "close_reason": "superseded|stopped|corrected|retracted_source|null",
  "close_authority": "explicit_end|caller|replayed_caller|successor_interval|heuristic|null",
  "arity_class": "set_valued|single_valued",   // engram_core::relations::relation_spec
  "safety": true
}
```

`superseded_by` / `superseded_at` keep their names, types and population rules (compat view,
interfaces.md §3.3). `id` is unchanged.

`conflict_entry` (`facts.rs:353-376`) gains:

```jsonc
{
  "handle": "conflict:<uuid>",
  // The next three are NULL on any queue row written before the v10 migration.
  // Null means "pre-v10 row", not "unknown" — §4.2 — and CLP-5 states that in
  // agent-info so a consumer branching on them has a defined case.
  "decision_key": "<subject_norm><relation><pair_hash>",
  "member_key": "<relation>:<object_norm>",
  "registry_version": "relations@1",
  "suggested_action": "keep-both|supersede-old|requires_context",   // R7
  "resolve": { "program": "engram", "args": [...], "allowed_actions": [...] },  // R12
  "resolve_command": "engram facts resolve <id> --action keep-both"             // R12, pipe-free
}
```

`decision_key` / `member_key` / `registry_version` are read from `FactConflictRow`, which gains
those three columns in wave 2 (storage-journal, interfaces.md §3.5 / §2 `conflict.opened`
payload). Request SR-4 in §6.2 makes them public fields on the row struct.

### 2.7 No SQL DDL is authored by this lane

Read-surfaces owns no `.sql` and no `sqlite.rs`. Every schema change it depends on is already
pinned in interfaces.md §2/§3 (events, `fact_decisions`, the `fact_conflict_queue` columns and
`idx_fcq_open_key`, the `facts.close_*` columns). The only DDL-adjacent *request* this lane makes
is one index, so `total` counts stay O(index) rather than O(table):

```sql
-- storage-journal to create alongside the wave-2 fact indexes (request SR-6, §6.2):
CREATE INDEX IF NOT EXISTS idx_facts_scope_open ON facts(kb, diary, valid_until, superseded_by);
```

**This must not become a second index for the same job.** storage-journal already plans
`idx_facts_open_interval … WHERE valid_until IS NULL AND superseded_by IS NULL` for open-fact
lookups. That partial index cannot serve `count_facts_scoped(include_superseded = true)`, which is
why SR-6 asks for a scope-leading one; but if storage prefers a single combined index, this lane
takes whatever storage ships and SR-6 becomes "one index that makes `COUNT(*)` over
`(kb, diary, open-ness)` index-only". Storage owns the shape; the requirement is the complexity
class, not the DDL. Two overlapping indexes for one query is storage-journal's call to refuse.

### 2.8 Paging on ranked reads — the one place `cursor` is deliberately absent

`recall`, `research` and `memory search` return a RANKED truncation, not a page. A cursor implies a
stable offset into a stable total order, and the order here depends on a network reranker whose
output is not guaranteed stable between calls. So on ranked surfaces:

- `returned` = results after the layer-token trim,
- `total` = `RecallStats::candidates_fused` (the pool the ranking chose from), documented as
  "candidates considered, not matches available",
- `cursor` = `null`, always, and `agent-info` says: to see more, raise `--top-k`.

Emitting a cursor there would be a lie with a stable-looking shape, which is worse than no cursor.
Listing surfaces (§3.3–§3.9) all get real cursors.

**Contested — see OQ-9.** Interfaces v2 pins `total` as "matching rows in scope". Setting it to
`candidates_fused` gives one field two meanings across surfaces, which is the defect class
constraint 3 exists to prevent, even though every individual value is honest. The alternative is
`data.candidates_fused` beside `total: null` (or no `total` at all) on ranked reads. The lead
rules before §3.3.5 and §3.5 are implemented.

---

## 3. File-by-file change plan

Line numbers are against `main` at 1d63d74; they will drift as other lanes land, so each item also
names its function.

### 3.1 `crates/engram-cli/src/read_surface.rs` (NEW, ~180 lines)

`CursorKey`, `encode_cursor`, `decode_cursor`, `next_cursor` (§2.1); `Provenance` (§2.4);
`absence_from(stats, scope) -> output::Absence` and `absence_reason(stats, req)` (§2.3.2);
`suggested_action_for(row_suggested, safety)` (R7, §3.3.6). Pure functions plus `serde_json`
rendering; no store access, no async, so it is unit-testable without a store.

It declares **no** envelope type. `Listing`, `Scope`, `Verdict` and `Absence` are
`output.rs`'s (contract-codegen §2.7) and are imported, never redefined — see §2. Module doc
explains WHY totals are separate from counts (the 953-open-conflicts defect) in the repo's
narrative register.

### 3.2 `crates/engram-cli/src/retrieval.rs`

| Site | Function | Change |
|---|---|---|
| `:292-301` | `struct HybridResult` | add `pub provenance: Provenance` |
| `:602-606` | `struct RecallOutcome` | add `pub stats: RecallStats` |
| new, beside `RecallNotes` | `struct RecallStats` | §2.5 |
| `:670-679` | `hybrid_recall` signature | unchanged |
| `~:717-758` | chunk load | after `chunk_index` is built, record `stats.chunks_considered = chunks.len()` and `stats.kbs_searched` (dedup of `chunks[].kb`) |
| `~:900-930` | fusion | record `stats.candidates_fused = fused.len()` and `stats.top_score = fused.first().map(|(_, s)| *s)` BEFORE the `rerank_top_n` truncation |
| `:920-924` and `:948-953` | both early empty returns | return `RecallOutcome { results: vec![], notes, stats }` — the stats are the whole point of the certificate, and `:920-924` (`runs.is_empty()`) is the site an implementer reading only §2.3.1 would miss |
| `:995-1020` | candidate materialization | build `Provenance` per §2.4; for `kind == "chunk"` read `memory_id` from `chunk_index.get(&id)`; for `claim` from `c.parent_chunk` + `chunk_index`; else `derived_from` |
| `:1055-1064` | `results.push(HybridResult{..})` and final `Ok` | carry `provenance`, carry `stats` |
| `:620-628` | `struct CandidateRecord` | add `memory_id: Option<Uuid>` so claim/wiki materialization does not re-query |

Ranking, RRF, `source_rank`, `ranked_run`, the diversity cap and `render_answer_context` are NOT
touched. `ANSWER_CONTEXT_FORMAT` stays `"engram-result/1"` — the frame's shape does not change.

### 3.3 `crates/engram-cli/src/commands/recall.rs`

1. **`run()` (`:19-289`)** — signature gains nothing; `RecallOutcome` destructuring at `:90-91`
   gains `stats`.
2. **Results JSON (`:123-141`)** — each row gains `"memory_id"`, `"handle"` (`chunk:<uuid>` /
   `claim:<uuid>` — `HandleKind` per interfaces.md §1) and `"provenance"`. `id`, `kind`, `score`,
   `rerank_score`, `kb`, `content`, `citations`, `sources` unchanged.
3. **Verdict + absence** — replace the `status` computation at `:249-253` with:
   ```rust
   let verdict = if results_json.is_empty() {
       output::Verdict::NoMatch(read_surface::absence_from(&stats, &scope))
   } else { output::Verdict::Results };
   let status = if results_json.is_empty() { "no_results" } else { "success" };  // unchanged
   ```
   and add `data.verdict`, plus `data.absence` (only on `NoMatch`) built from `stats` + `scope`.
4. **Scope** — build `output::Scope` from `scope` (`retrieval.rs:472-495`), `diary`, and
   `stats.chunks_considered`; emit as `metadata.scope`. The existing flat `kb`, `kb_scope`,
   `diary`, `diary_isolation`, `temporal_scope` metadata fields all stay (constraint 3).
5. **Paging** — `data.returned`, `data.total` (= `stats.candidates_fused`), `data.cursor` = null
   per §2.8. Legacy `metadata.results_returned` (`:238`) stays.
6. **`surface_conflicts()` (`:296-353`)** — the deep change:
   - signature becomes
     `fn surface_conflicts(ctx, kb: Option<&str>, diary: Option<&str>, query: &str, limit: usize) -> Result<ConflictSurface, CliError>`
     where `ConflictSurface { items: Vec<Value>, total: i64 }`. **Deliverable 5**: `diary` is
     threaded into both store calls (SR-2, SR-3) — today a `--diary work` recall can surface a
     conflict between two `personal` facts, which is a cross-diary leak on the exact surface that
     exists to prevent silent wrong answers.
   - **`:310` — the sweep site.** `let Some(superseded_by) = f.superseded_by else { continue };`
     becomes `if is_open_fact(&f) { continue; }` (`engram_core::interval::is_open_fact`), and the
     rendered `status` comes from `f.close_reason`:
     `superseded → "superseded"`, `stopped → "ended"`, `corrected → "corrected"`,
     `retracted_source → "retracted"`, `None → "closed"`. **This widens an existing field's value
     set** — today `:321-338` emits only `"superseded"` | `"contested"` — so it is a documented
     change, not a silent one: CLP-5 adds the full vocabulary to `agent-info` and CLP-6 puts it in
     the CHANGELOG in the same wave. `"contested"` keeps its current meaning and is unchanged.
     `new_object` is looked up from
     `f.closed_by` when present, falling back to `f.superseded_by` (compat view) — a fact closed
     because the user stopped a medication has NO successor, and printing an empty `new_object` as
     if it did is precisely the "stopped medication reads as current" failure inverted.
   - each side gains `memory_id`, `handle`, `provenance`, `open`, `valid_from`, `valid_until`.
   - **`:344` `suggested_action`** now comes from the row, and is passed through the R7 guard
     `read_surface::suggested_action_for(row_suggested, safety)`: if either side's canonical
     relation `is_safety_slot`, the emitted value is `"requires_context"` regardless of what the
     row says. This is defence in depth against a stale queue row written before fact-semantics
     landed (there are 951 of them in a real store).
     **R7 is the binding text here and it overrides `spec-fact-semantics.md` §1.4**, which still
     describes a blanket withdrawal of `supersede-old`. Non-safety single-valued-strict pairs may
     still suggest `supersede-old`; only safety slots are forced to `requires_context`. An
     implementer reading both documents follows R7.
   - **`:345-348` `resolve_command`** is replaced by the R12 builder, using the API
     contract-codegen actually ships (§2.4 of that spec — there is no `resolve_payload`):
     ```rust
     let payload = respond::resolve_conflict(qid, allowed_actions);   // ResolvePayload
     respond::attach(&mut entry, &payload);                            // writes BOTH keys
     ```
     `attach` renders `resolve { program, args, allowed_actions }` and the legacy
     `resolve_command` string containing ONE runnable command with no pipes. `allowed_actions:
     &[&'static str]` comes from the caller per R12 — here from
     `engram_graph::semantics::allowed_actions(verdict, new, old)` (FS-3). Note that
     `resolve_command_string` uses `allowed_actions[0]`, and contract-codegen orders
     `keep-both` first deliberately because it is the only universally safe blind action; this lane
     must not reorder the slice to put `suggested_action` first.
     `"close-old"` may appear in `allowed_actions` only after CC-R7 widens the `resolve_action`
     vocabulary (§6.4) — until then `allowed_actions_are_from_the_vocabulary` fails the build.
   - `conflicts` in the payload (`:261`) becomes
     `{"returned": n, "total": t, "items": [...]}`; the bare array shape is kept at
     `data.conflicts` for one release **only if** contract-codegen agrees — otherwise the array
     stays at `data.conflicts` and the counts go to `data.conflicts_returned` /
     `data.conflicts_total`. **Decision: take the second form.** It is strictly additive and no
     consumer breaks. (Open question OQ-3.)
7. **`query_terms()` (`:357-374`)** — unchanged.
8. `parse_layer`, `parse_optional_time` and their tests — unchanged.

### 3.4 `crates/engram-cli/src/commands/facts.rs`

1. **Imports (`:20-24`) — ruling R3.** Becomes:
   ```rust
   use crate::fact_write::{
       classify_pair, write_fact_batch, Autonomy, ClassifyMode, ConflictClass, FactBatch,
       FactExtractionMarker, FactWriteCtx, FactWriteOutcome, MemoryTime, Verdict,
   };
   use engram_core::interval::is_open_fact;
   use engram_core::relations::{
       canonical_relation, is_safety_slot, predicate_arity, relation_spec, Arity, LegacyArity,
   };
   ```
   `Arity` in this file means exactly `engram_core::relations::Arity { SetValued, SingleValued }`;
   the two-variant legacy projection is `LegacyArity` and reaches the file only through
   `predicate_arity`. Nothing is imported as `Arity` from `fact_write`. `is_safety_slot` and
   `canonical_relation` come from `engram-core`, not from `fact_write`'s re-export, so this file
   has one home per name (CM-4, CW-6).
2. **`list()` (`:44-142`)** — gains `cursor: Option<String>` (from CLP-2) and emits
   `returned` / `total` / `cursor` via `output::Listing`. `total` from `count_facts_scoped` (SR-7); `count`
   stays = `returned`. Each row gains the §2.6 fields.
   **The `--subject` branch (`:63-72`) is a separate query with no limit at all today**
   (`list_facts_by_subject_scoped`). SR-7 therefore adds `limit` AND `offset` to it, not offset
   alone: a cursor over an unbounded query is nonsense, and until the limit exists that branch must
   emit `total == returned` with `cursor: null` and say so rather than pretend it paged.
   **`:123` — sweep site**: the human renderer's `if f["superseded_by"].is_null()` becomes
   `if f["open"] == json!(true)`, and the label widens from `" [superseded]"` to
   `" [closed: <close_reason>]"`. A stopped medication must not print as an active one.
3. **`show()` (`:144-197`)** — **`:156-157` sweep sites**:
   ```rust
   let active: Vec<&Fact> = facts.iter().filter(|f| is_open_fact(f)).collect();
   let historic: Vec<&Fact> = facts.iter().filter(|f| !is_open_fact(f)).collect();
   ```
   The human "History (superseded):" block reads `valid_until` first and `superseded_at` only as
   a fallback, and prints the close reason. `active_count`/`historic_count` keep their names;
   `returned`/`total` are added for both arrays (`total` == `returned` here — `show` is
   unlimited by construction, and saying so is honest).
4. **`conflicts()` (`:199-263`)** — **deliverable 5**: `list_recent_conflicts(limit)` becomes
   `list_recent_conflicts(diary, kb, limit)` (SR-3) so the history view stops ignoring both
   scopes it is handed. Adds `returned`/`total` (`count_recent_conflicts`, SR-3) and `cursor`.
   `open_count` and `next_action` are unchanged.
5. **`open_conflicts()` (`:270-349`)** — `total_open` (already correct, `:283-285`) becomes the
   canonical `total`; add `returned` and `cursor`; keep `count` and `total_open` as aliases.
   The human tail at `:341-346` already prints "N of TOTAL", which is the behaviour the rest of
   the surface is being brought up to.
6. **`conflict_entry()` (`:353-376`)** — §2.6 fields; `suggested_action` through the R7 guard;
   `resolve_command` + `resolve {}` from `respond.rs` (R12). The `triage` sub-object keeps its
   current shape.
7. **`group_conflicts()` (`:407-447`)** — the `arity` match at `:436-439` becomes
   ```rust
   "arity": match predicate_arity(&canonical) {           // LegacyArity, unchanged JSON
       LegacyArity::Single => "single",
       LegacyArity::Multi  => "multi",
   },
   "arity_class": match relation_spec(&canonical).arity {  // NEW
       Arity::SetValued { .. }    => "set_valued",
       Arity::SingleValued { .. } => "single_valued",
   },
   "safety": is_safety_slot(&canonical),                   // NEW
   ```
   `safety_slot` (`:440`) keeps its name and value — `agent-info` documents it and a rename would
   silently break a documented field (CW-6).
8. **`resolve()` (`:451-517`)** — four changes:
   - **claim the row and record the decision in ONE storage call.** Today `:459-467` reads the row,
     then `:504` resolves it: two concurrent `facts resolve` calls both pass the `status != "open"`
     check and can supersede both facts of a pair, reporting success twice (brief move 4,
     `facts.rs:451-506`). storage-journal §2.11 already ships the atomic API for exactly this, so
     this lane calls it rather than inventing a second claim path:
     ```rust
     let key = SemanticKey::for_pair(&new_view, &old_view);          // fact-semantics, FS-2
     match ctx.store.resolve_conflict_with_decision(
         qid, kb, diary, &key.encode(), verdict, "caller", actor, note, SEMANTIC_KEY_VERSION)? {
         None => return Err(CliError::ConflictAlreadyResolved {         // exit 3
             conflict_id: qid.to_string(), verdict: prior_verdict },),
         Some(decision_id) => { /* apply the effect, below */ }
     }
     ```
     `Ok(None)` means somebody else won the claim. There is **no** `claim_fact_conflict`: a second
     claim path would reopen the race at the seam between the two calls.
   - **the decision is recorded for every action, including `keep-both`, which today writes
     literally nothing** — that omission is why move 3's replay has nothing to replay (CD-3).
     `resolve_conflict_with_decision` writes it as part of the same transaction, so "resolved with
     no decision" stops being representable.
   - **the effect must land in the same transaction as the claim.** `supersede_fact_with` /
     `close_fact_interval` are called from inside the caller's `with_tx` (nesting on `self.conn`
     is what `sqlite.rs:835-836` already does). **If the effect call returns `false` after a
     successful claim, the transaction is rolled back and the command exits non-zero**; a committed
     `resolved` queue row with no closed interval is a silent corruption, and "the claim succeeded
     so we report success" is the failure this deliverable removes. SR-10 (§6.2) asks storage to
     confirm the composition is legal under its own `with_tx`.
   - **CLI action → journal verdict.** The CLI vocabulary and R4's `decision.recorded` vocabulary
     are different strings and must be mapped explicitly; a wrong string means a decision that
     never replays (CD-3, half-fixed is worse than unfixed):

     | `--action` (CLI, `resolve_action`) | `verdict` written to `decision.recorded` (R4) | effect |
     |---|---|---|
     | `keep-both` | `keep_both` | none; both intervals stay open |
     | `supersede-old` | `close_old` | `supersede_fact_with(old, new, "caller", …)` |
     | `reject-new` | `reject_new` | the new fact is not written / is closed |
     | `close-old` (§6.4 CC-R7) | `close_old` | `close_fact_interval(old, at, "stopped", "caller", None, actor)` |
     | — (machine sweep, `resolve_orphan_fact_conflicts`) | `void` | storage-journal's, not this lane's |

     `close-old` and `supersede-old` share the `close_old` verdict deliberately: R4's vocabulary
     names the *decision*, and the difference between them is the effect (successor vs no
     successor), which the event's own payload records.
   - **`--as-of` / `close-old`** (spec-fact-semantics §6.4.5, optional-but-recommended): accept
     `--action close-old --as-of <RFC3339|YYYY-MM-DD>`. Date parsing calls
     `engram_core::temporal::{parse_bound, DayBound}` directly — the same function `recall.rs:13`
     already imports. It is **not** re-homed into `read_surface`; core already owns it and a CLI
     copy would be a second home for one meaning. Requires CLP-2's `--as-of` flag and CC-R7.
   - **payload through `respond.rs`** (R12) — `respond::attach(&mut obj, &respond::resolve_conflict(
     qid, allowed_actions))`.
   `supersede_fact_with(old, new, "caller", ...)` at `:480` and `:492` keeps its `"caller"`
   resolution tag; storage-journal must NOT hard-code `authority="heuristic"` inside it (CW-4).
   That is request SR-8.
9. **`triage()` (`:536-764`)** — the inline safety defence at `:581-593` is replaced by
   `engram_graph::semantics::safety_gate(verdict, &new_view, &old_view)` (spec-fact-semantics
   §6.4.1). The rule name stays `"safety_slot"` because `:701` reads it as a map key. Everything
   else in `triage` — the settled-facts pass, dry-run honesty, the transaction, the orphan sweep —
   is unchanged.
10. **`fact_to_json()` (`:1094-1115`)** — §2.6.
11. **`backfill()` (`:818-1063`)** — unchanged except that its `adjudication_needed` entries flow
    from `fact_write`'s queue payload, which fact-semantics is already updating; no edit here.

### 3.5 `crates/engram-cli/src/commands/research.rs`

Same additive treatment as recall, minus the conflicts array (research is an evidence pass, not an
adjudication surface):
- `results_json` (`:127-147`) gains `memory_id`, `handle`, `provenance`;
- `data.verdict` + `data.absence`, `metadata.scope`, `returned` / `total` / `cursor: null`;
- `verification` block (`:179-184`) keeps its shape and gains `"uncited_results"` unchanged;
- KB-scope default is NOT changed here (research defaults to `--kb`, recall to every KB). Making
  them agree is a behaviour change outside this lane's mandate — OQ-2.

### 3.6 `crates/engram-cli/src/commands/people.rs`

- `list()` (`:20-…`) — `output::Listing` over `list_entities_by_kind`; `total` from `count_entities_by_kind`
  (SR-9); `cursor`; `people` array unchanged per row.
- `show()` (`:106-…`) — the `memories` array already carries `memory_id` and `chunk_id`
  (`:384-389`); add `handle` (`mem:<uuid>`) and a `provenance` object so it matches every other
  surface. `relations` rows gain `handle` for their entity ids.
- `memory_snippets()` (`:367-440`) — one line per pushed row; no query changes.
- No diary threading: entities and relations carry no diary column. `metadata.scope.diary` is
  reported as `"*"` with `scope.diary_applicable: false` rather than pretending a filter ran.

### 3.7 `crates/engram-cli/src/commands/entities.rs`

- `list()` (`:8-33`) — `output::Listing`; `total` from SR-9; `cursor`; keep `entities_returned` metadata.
- `show()` (`:35-59`) — `entity` gains `handle` (`entity:<uuid>`); `neighbors` gains
  `returned`/`total`.

### 3.8 `crates/engram-cli/src/commands/graph.rs` and `wiki.rs`, `documents.rs`

- `graph::neighbors()` (`:18-188`) — the `neighbors` array is already capped by `limit` at
  `:123-127` while `relations` is not; emit `returned` = neighbours after the cap, `total` =
  neighbours before it, `cursor` over the same stable sort at `:115-122`. Every relation row gains
  `handle` for `from_entity_id` / `to_entity_id`. `sources_of()` (`:198-207`) is unchanged.
- `wiki::run()` (`:8-29`) — list branch gains `Listing` (`total` = `list_wiki_pages` length; the
  query is unbounded today, so `total == returned` and saying so is honest). The page branch gains
  `handle` **only if `HandleKind` grows a `wiki` variant** — it has none today and neither the
  interfaces §1 grammar nor CC-R2's prefix list mentions one. Requested as CC-R2b (§6.4). If the
  lead refuses it (a wiki page is not resolvable by any mutator, so a handle would be decorative),
  the page branch emits `page_id` and no `handle`, and §4.1's "every row carries a handle" is
  scoped to resolvable rows. Do not hand-build a `wiki:` prefix that `resolve_handle` will reject.
- `documents::list()` (`:9-26`) — `output::Listing`, `total` from SR-9; each document gains
  `handle` (`doc:<uuid>`). `documents::show()` (`:28-40`) gains `handle` and a `provenance`
  summary (`memory_ids_for_document`, `sqlite.rs:2601`) capped and counted.
  `documents::delete()` is a mutator and is untouched by this lane.

### 3.9 `crates/engram-cli/src/commands/memory.rs` — write-surfaces' file, request only

This lane does not edit it (§1.3). **WS-1** (§6.5) asks write-surfaces, which is already rewriting
this file, to give `list()` the same treatment as every other listing: `output::Listing` with
`total` from `count_memories_scoped` (SR-9) and `cursor` from `read_surface::next_cursor`, plus the
per-row `handle` (`mem:<uuid>`) that spec-write-surfaces §3.1 already plans. §5.2's
`every_listing_surface_reports_returned_total` covers `memory list` and is `#[ignore]`d until WS-1
lands (§5 gating rule).

### 3.10 `memory search` ≡ `recall` (deliverable 6)

The two commands diverge only in `lib.rs:232-257`, which hard-codes `layer="topic"`,
`rerank_top_n=None`, `graph_hops=1`, `allow_mixed_embeddings=false` and a different default
`--mode` (`agent` vs `evidence`). The fix is a single call-shape change in a file this lane does
not own, so it is **CLP-3**: `MemoryCommand::Search` forwards to `commands::recall::run` through a
`RecallArgs` struct that this lane defines and exports from `recall.rs`:

```rust
// crates/engram-cli/src/commands/recall.rs — this lane owns this
pub struct RecallArgs {
    pub query: String, pub top_k: usize, pub layer: String, pub mode: String,
    pub profile: String, pub kb: Option<String>, pub all_kbs: bool, pub diary: String,
    pub rerank_top_n: Option<usize>, pub graph_hops: u8, pub allow_mixed_embeddings: bool,
    pub since: Option<String>, pub until: Option<String>,
    /// Which spelling the caller used. Reported as `metadata.command_alias`, so
    /// a response is traceable to the surface that produced it; it changes NO
    /// behaviour.
    pub alias: &'static str,   // "recall" | "memory search"
}
pub async fn run(ctx: &AppContext, args: RecallArgs) -> Result<(), CliError>;
```

The 14-positional-argument signature at `recall.rs:19-34` (which already carries
`#[allow(clippy::too_many_arguments)]`) goes away; both dispatch arms build `RecallArgs`. Default
divergences that remain are then visible in one place, in `cli.rs`, where contract-codegen can
generate them from one schema. `memory search`'s current defaults are PRESERVED (`--mode agent`)
so no behaviour changes in wave 3; the reconciliation is structural, and OQ-4 asks the lead
whether the defaults should be unified in wave 4.

---

## 4. Migration and compatibility

This lane authors no schema migration. Its compatibility obligations are on the JSON surface and
on read behaviour across a v9→v10 store transition.

### 4.1 Additive-only contract (brief constraint 3)

| Field | Rule |
|---|---|
| `id` | unchanged name, unchanged type (bare uuid string), unchanged meaning on every row |
| `count` | KEPT on every listing, always equal to `returned` |
| `total_open`, `open_count`, `results_returned`, `entities_returned`, `people`, `pages`, `documents` | KEPT with current meanings; `total`/`returned` are added beside them |
| `superseded_by`, `superseded_at` | KEPT and still populated when `close_reason == 'superseded'` (interfaces.md §3.3 compat view) |
| `resolution` | KEPT; `close_reason`/`close_authority` are the structured successors |
| `suggested_action` | vocabulary UNCHANGED (`keep-both`/`supersede-old`/`requires_context`, R7); only which value is chosen changes |
| `resolve_command` | KEPT as a string; now exactly one runnable command with no pipes (R12) |
| `status` on recall/research | KEPT (`success`/`no_results`); `verdict` is the new machine field |
| `status` on a **conflict entry** | name and type KEPT; the VALUE SET widens from `superseded`/`contested` to add `ended`/`corrected`/`retracted`/`closed` (§3.3.6). This is the one value-semantics change in the lane and it is documented in agent-info (CLP-5) and the CHANGELOG (CLP-6), never shipped silently |
| `answer_context` | shape and `ANSWER_CONTEXT_FORMAT` unchanged |

New fields on this lane's surfaces: `returned`, `total`, `cursor`, `verdict`, `absence`,
`memory_id`, `handle`, `provenance`, `open`, `closed_by`, `close_reason`, `close_authority`,
`arity_class`, `safety`, `decision_key`, `member_key`, `registry_version`, `resolve`,
`metadata.scope`, `metadata.scope.origin`, `metadata.command_alias`. Every one is documented by contract-codegen in the
same wave (CD-10) or the generated contract test fails on an undocumented field — that
coordination is CLP-5.

### 4.2 Reading a store that has not been migrated yet

Waves overlap. Read surfaces must not panic or lie against a v9 store:

- `close_reason` / `close_authority` / `closed_by` are `Option` with `#[serde(default)]`
  (interfaces.md §3.3), so a v9 row deserializes with all three `None`.
- `is_open_fact(f)` on a v9 row is `valid_until.is_none() && superseded_by.is_none()` — which is
  exactly today's `superseded_by.is_none()` for rows that never had `valid_until` set. The sweep
  is therefore behaviour-preserving on unmigrated data and correct on migrated data. This is the
  property test in §5.6.
- `decision_key` / `member_key` / `registry_version` render as `null` when the queue row predates
  the wave-2 columns. A consumer that requires them is reading a pre-migration store and the null
  says so.
- `arity_class` / `safety` are computed from the registry at read time, not stored, so they are
  correct on any store.

### 4.3 Behaviour changes a caller can observe (and why each is required)

1. `facts show` moves a fact from `active` to `historic` when `valid_until` is set but
   `superseded_by` is not. That is the bug being fixed: a stopped medication currently reads as
   current (brief move 2 / CD-2).
2. `recall`'s conflicts array is now diary-scoped. A `--diary work` recall stops reporting
   `personal` conflicts. This narrows output; it is a leak fix, and `metadata.scope` explains the
   narrowing.
3. Safety pairs never suggest `supersede-old`, including for queue rows written before wave 3
   (§3.3.6). A caller that blindly executed `suggested_action` on a medication conflict stops
   auto-closing it. That is the entire point of R7.
4. `facts resolve` on an already-claimed row now exits 3 with `ConflictAlreadyResolved` instead of
   racing to a double success. Sequential callers are unaffected.
5. `facts conflicts` (history view) now honours `--kb`/`--diary`, which it accepted and ignored.

Items 2, 3 and 5 change what a caller SEES; each is a correctness fix and each is named in the
CHANGELOG entry this lane writes (CLP-6 — `CHANGELOG.md` is lead-owned at integration).

### 4.4 `bench personal`

`bench.rs` is frozen and not touched. `hybrid_recall`'s ranking is not touched. The only change
inside the retrieval path is bookkeeping (`RecallStats`) and one struct field. §5.9 pins that
`engram bench personal --json` produces identical scores before and after.

---

## 5. Test plan

**Isolation rule, non-negotiable (brief constraint 1, interfaces.md §5 / R14): every test sets
`ENGRAM_STORE` to a `TempDir`.** New tests use `ENGRAM_STORE` alone — no `HOME`/`XDG` tricks.
Provider keys are stripped so no test can make a network call.

```rust
// shared preamble for every new test file in this lane
fn store_dir() -> tempfile::TempDir { tempfile::tempdir().expect("tmp dir") }

fn engram(store: &tempfile::TempDir) -> assert_cmd::Command {
    let mut cmd = assert_cmd::Command::cargo_bin("engram").expect("binary built");
    cmd.env("ENGRAM_STORE", store.path());
    for k in ["GEMINI_API_KEY", "COHERE_API_KEY", "OPENROUTER_API_KEY",
              "ENGRAM_COMPILER_LLM_STUB", "ENGRAM_FACT_LLM_STUB"] {
        cmd.env_remove(k);
    }
    cmd.env("ENGRAM_BENCH_FORCE_STUB", "1");
    cmd
}
```

**Cross-lane gating rule.** Any test here that exercises another lane's surface is `#[ignore]`d
with the blocking request named in the ignore reason (`#[ignore = "blocked on WS-1"]`), and the
lead's wave-3 integration gate un-ignores it. Unmarked cross-lane tests are red for reasons this
lane cannot fix, which trains the lane to ignore reds. The gated set: §5.1.2 and §5.1.3
(WS-2 — `forget`/`edit` accepting `memory_id` and kind-prefixed handles), §5.2.6's
`memory list` row (WS-1), §5.5 (storage's `resolve_conflict_with_decision`, SR-10), and all of
§5.6 (the lead-owned sweep gate).

A test that constructs an `AppContext` in-process uses `SqliteStore::open_in_memory()` or a
`TempDir` + `ENGRAM_STORE`; neither may fall back to `ProjectDirs`. A reviewer rejects any test in
this lane that does not name `ENGRAM_STORE` or `open_in_memory`.

### 5.1 `tests/wave3_read_provenance.rs`

1. `recall_hit_carries_memory_id_and_handle` — remember two memories offline, recall, assert every
   `results[]` row has non-null `memory_id`, `handle` matching `^(chunk|claim):[0-9a-f-]{36}$`,
   and `provenance.kb` equal to the row's `kb`.
2. `recall_memory_id_is_accepted_by_a_mutator` — `#[ignore = "blocked on WS-2"]`.
   Take `results[0].memory_id`, run
   `engram forget <id> --confirm`, assert exit 0 and `deleted: true`. This is the brief's headline
   defect ("recall returns chunk ids that edit/forget reject") and the test that proves it closed.
3. `recall_handle_round_trips` — `#[ignore = "blocked on WS-2"]`.
   `edit <handle-with-prefix> --content …` succeeds (interfaces.md §1 accepts kind-prefixed and
   bare).
4. `derived_hits_declare_their_derivation` — a wiki/entity hit has `memory_id: null` and
   `provenance.derived_from` set; assert it is never a fabricated uuid.
5. `facts_list_rows_carry_memory_id_and_fact_handle`.
6. `people_show_snippets_carry_handles`.

### 5.2 `tests/wave3_read_paging.rs`

1. `facts_list_total_exceeds_page` — write 25 facts (via `--facts-json`), `facts list --limit 5`,
   assert `returned == 5`, `total == 25`, `count == 5`, `cursor` present.
2. `cursor_walks_the_whole_set_exactly_once` — follow cursors to exhaustion; assert the union of
   ids has 25 members, no duplicates, and the final page has `cursor: null`.
3. `cursor_bound_to_scope` — a cursor issued for `--kb a` passed with `--kb b` exits 3 with a
   message naming the mismatch.
4. `malformed_cursor_is_exit_3` — `--cursor garbage`.
5. `conflicts_total_is_the_backlog_not_the_page` — queue 12 conflicts, `facts conflicts --open
   --limit 3`, assert `returned == 3`, `total == 12`, `total_open == 12`. This is the
   "3 conflicts vs 953" defect.
6. `every_listing_surface_reports_returned_total` — table-driven over
   `facts list`, `facts conflicts`, `facts conflicts --open`, `people list`, `entities list`,
   `documents list`, `wiki`, `memory list`; assert all three keys exist and `count == returned`.
7. `ranked_reads_report_null_cursor` — `recall` and `research` have `cursor: null`. The companion
   assertion on `total` is written only after OQ-9 is ruled (§2.8); until then the test asserts
   the cursor rule alone.

### 5.3 `tests/wave3_read_absence.rs`

1. `empty_store_recall_is_no_match_with_a_certificate` — `verdict == "no_match"`,
   `absence.reason == "no_chunks_in_scope"`, `absence.top_score == null`,
   `absence.searched_chunks == 0`, `status == "no_results"` (unchanged).
2. `wrong_kb_recall_names_the_kb_it_searched` — write to `--kb work`, recall `--kb personal`;
   `absence.searched_kbs == ["personal"]`. Proves an agent can tell a scoping mistake from an
   empty store.
3. `threshold_is_null_or_absent_when_no_cutoff_ran` — asserts the honesty rule of §2.3.1: the key
   is either absent or `null`, never `0.0`. Written after OQ-10 is ruled; it fails loudly if
   someone later fabricates a zero.
6. `absence_reason_matches_the_decision_procedure` — table-driven over §2.3.2's five rows:
   empty store → `no_chunks_in_scope`; `--since` in the future → `filtered_by_window`;
   `--diary` with no rows → `filtered_by_diary`; and the two non-empty-pool cases.
4. `results_verdict_has_no_absence_key`.
5. `scope_metadata_present_on_every_read` — table-driven over all nine read surfaces.

### 5.4 `tests/wave3_read_conflicts.rs`

1. `safety_pair_always_requires_context` — write `takes_medication` twice with different objects,
   `recall` the subject; assert every conflict entry has
   `suggested_action == "requires_context"`. Repeat against a conflict row whose stored
   `suggested` is `"supersede-old"` (insert via a pre-wave-3 path fixture) to prove the read-side
   guard, not just the write-side one (R7 defence in depth).
2. `non_safety_single_valued_strict_pair_may_suggest_supersede_old` — the honest half of R7.
3. `conflicts_respect_diary` — two conflicts in diary `a`, recall with `--diary b`, assert zero
   conflicts and `metadata.scope.diary == "b"`.
4. `conflicts_respect_kb` — same for `--kb`.
5. `resolve_command_is_pipe_free_and_runnable` — assert `resolve_command` contains no `|`, split
   it and run it, assert exit 0. Today's string contains `supersede-old|keep-both|reject-new`,
   which is not a command.
6. `resolve_payload_lists_allowed_actions` — `resolve.allowed_actions` is a non-empty array and
   contains `suggested_action` whenever `suggested_action != "requires_context"`.
7. `closed_interval_without_successor_reports_ended_not_superseded` — close an interval with
   reason `stopped`; assert `status == "ended"` and `new_object` absent/null rather than `""`.

### 5.5 `tests/wave3_read_resolve.rs`

1. `resolve_records_a_decision_for_every_action` — for each of `supersede-old`, `keep-both`,
   `reject-new`, assert an event of kind `decision.recorded` exists afterwards **and that its
   `verdict` is the R4 spelling from §3.4.8's mapping table** (`close_old`, `keep_both`,
   `reject_new` — not the CLI hyphenated form; a wrong string is a decision that never replays)
   (via
   `engram admin events --subject <id>` if storage-journal ships a reader, else via a direct
   `SqliteStore` assertion in an integration test with `ENGRAM_STORE`). `keep-both` is the case
   that writes nothing today (CD-3).
2. `second_resolve_of_the_same_conflict_is_exit_3` — `ConflictAlreadyResolved`.
3. `concurrent_resolve_produces_exactly_one_winner` — **two processes, not two threads.** A
   `SqliteStore` owns a single `Connection`; sharing one across threads does not reproduce the
   race the brief names and would teach the wrong concurrency story. Spawn two
   `assert_cmd` children against the same `ENGRAM_STORE`, released together, and assert exactly
   one exit 0 and one exit 3, and that exactly one fact is closed (never both). Because SQLite may
   also return `SQLITE_BUSY` here, the loser is accepted as either exit 3
   (`ConflictAlreadyResolved`) or the `SqliteBusy` code retried once — an assertion that only
   permits exit 3 is flaky by construction.
4. `close_old_with_as_of_sets_valid_until_to_the_stated_date` (if §3.4.8's `--as-of` ships).

### 5.6 `tests/wave3_read_open_facts.rs` — the sweep (gate-owned)

Marked `#[ignore]` ONLY until the lead's named wave-3 sweep gate runs, per CD-2/CC-4; the gate
un-ignores these together with fact-semantics' §5.42 tests.

1. `stopped_fact_is_historic_in_facts_show` — set `valid_until` with `superseded_by` null; assert
   the fact appears under `historic`, not `active`.
2. `stopped_fact_is_not_labelled_active_in_facts_list_human_output`.
3. `stopped_fact_surfaces_in_recall_conflicts_as_closed`.
4. `open_fact_definition_is_stable_on_a_v9_shaped_row` — a row with `valid_until = NULL` and
   `superseded_by = NULL` is open; a row with only `superseded_by` set is closed. Proves §4.2's
   backward-compatibility claim.

### 5.7 `tests/wave3_read_search_parity.rs`

1. `memory_search_and_recall_return_the_same_envelope_keys` — same store, same query; assert the
   key sets of `data` and `metadata` are identical (values may differ by mode).
2. `memory_search_reports_its_alias` — `metadata.command_alias == "memory search"`.
3. `search_alias_of_recall_is_unchanged` — `engram search` (the `visible_alias` at `cli.rs:105`)
   behaves exactly as `recall`.

### 5.8 Unit tests inside the lane's own modules

- `read_surface.rs`: cursor round-trip; cursor rejects a foreign fingerprint; `next_cursor`
  returns `None` when `offset + returned == total`; `output::Listing::attach` writes
  `count == returned`; `absence_reason` returns each of §2.3.2's five values for the stats that
  define it, and never `no_candidate_above_cutoff`.
- `recall.rs`: existing `parse_optional_time` tests unchanged; add
  `suggested_action_for_safety_is_always_requires_context` as a pure unit test.
- `retrieval.rs`: `RecallStats` populated on the empty-pool early return (in-memory store).

### 5.9 Gates

- `cargo build --workspace`, `cargo clippy --workspace -- -D warnings`, `cargo fmt --check`,
  `cargo test --workspace` green in the worktree before every commit.
- `engram bench personal --json` compared before/after: identical scores. Run with
  `ENGRAM_STORE` pointed at a TempDir.
- No test in this lane opens `~/Library/Application Support/bio.199-biotechnologies.engram/`.
  Verified by grepping the lane's test files for `HOME`/`ProjectDirs` and finding none.

---

## 6. cross_lane_patches

### 6.1 One-line patches for the lead's patch channel (interfaces.md §7 / R15)

| id | file | patch | why |
|---|---|---|---|
| **CLP-1** | `crates/engram-cli/src/lib.rs` (after `pub mod output;`) | `pub mod read_surface;` | one home for paging/scope/provenance types |
| **CLP-2** | `crates/engram-cli/src/cli.rs` (contract-codegen) | listing paging flags — see §6.1.1 | `--cursor` cannot exist without a clap arg |
| **CLP-3** | `crates/engram-cli/src/lib.rs:120-160, 232-257` (daemon-jobs) | both `Recall` and `MemoryCommand::Search` arms build `commands::recall::RecallArgs { …, alias }` and call `recall::run(&ctx, args)` | deliverable 6; kills the 14-positional signature |
| **CLP-4** | `crates/engram-cli/src/lib.rs` facts arms `:307-337` | `FactsCommand::{List,Conflicts}` forward the new `cursor` / `offset` args; `Resolve` forwards `as_of` | wiring only |
| **CLP-5** | `crates/engram-cli/src/agent_info.rs` + `crates/engram-cli/assets/skill.md` (contract-codegen) | document the §4.1 new-field list, the opaque-cursor rule, the ranked-read `cursor: null` rule, R7's safety wording (replacing "Safety slots … are never auto-superseded", `agent_info.rs:41`), the widened conflict `status` value set (§3.3.6), and the rule that `decision_key`/`member_key`/`registry_version` are `null` on pre-v10 queue rows (§4.2) | CD-10: an undocumented field fails the generated contract test |
| **CLP-6** | `CHANGELOG.md` (lead) | the four observable behaviour changes in §4.3 | CW-7 leaves the CHANGELOG line unowned |
| **CLP-7** | `crates/engram-cli/src/error.rs` (contract-codegen) | none — the variant is already pinned as `CliError::ConflictAlreadyResolved { conflict_id: String, verdict: String }` (spec-contract-codegen §2.3), exit 3. This row records that **this lane constructs the struct variant, not a tuple variant**; call sites in §3.4.8 use the field form | §3.4.8's lost-claim path |
| **CLP-8** | `crates/engram-cli/src/commands/serve.rs` (daemon-jobs, wave 4) | `/v1/recall` calls the same builders instead of its bespoke JSON at `:577-610` | CM-9; noted, not requested for wave 3 |
| **CLP-9** | `crates/engram-cli/Cargo.toml` | `base64.workspace = true` | cursor encoding; already a workspace dep (`Cargo.toml:49`) |

#### 6.1.1 CLP-2 in full

```rust
// FactsCommand::List, FactsCommand::Conflicts, PeopleCommand::List,
// EntitiesCommand::List, DocumentsCommand::List, MemoryCommand::List:
/// Opaque continuation token from a previous page's `cursor`. Do not construct
/// or parse it; pass it back verbatim.
#[arg(long)]
cursor: Option<String>,

// FactsCommand::Resolve additionally:
/// For `--action close-old`: the date the interval ended (RFC3339 or
/// YYYY-MM-DD). Without it the interval closes at the successor's start.
#[arg(long)]
as_of: Option<String>,
```

`--action close-old` needs no clap change (action is already a free `String` validated in
`facts::resolve`).

### 6.2 Storage API requests (storage-journal, `engram-storage`)

These are NOT one-liners and are therefore interface requests, per the brief's "if the interface is
missing something you need, note it in your spec; do not invent a divergent one". Each names the
deliverable it blocks. Signatures follow the shape of the existing neighbours in `sqlite.rs`.

**Acceptance gate, stated plainly: none of SR-1…SR-3, SR-6, SR-7, SR-9 appear in
`spec-storage-journal.md` §1.1's deliverable list or in interfaces.md today.** They are requests,
not pins, and §2.7's "already pinned in interfaces.md §2/§3" refers to the *schema* (events,
`fact_decisions`, the queue columns, the `facts.close_*` columns), not to these read methods.
Deliverables 1, 2 and 5 do not compile until storage-journal accepts them. The lead schedules that
acceptance before wave 3 starts, or those three deliverables slip; R-1's build order assumes
acceptance, not slippage.

```rust
impl SqliteStore {
    // SR-1 — provenance batch lookup (deliverable 1). One query per response.
    pub fn memory_ids_for_chunks(&self, chunk_ids: &[Uuid])
        -> Result<HashMap<Uuid, Uuid>, StorageError>;

    // SR-2 — diary threading on the recall conflict surface (deliverable 5).
    // Today: facts_matching_terms(kb, terms, limit) — sqlite.rs:4032.
    pub fn facts_matching_terms(&self, kb: Option<&str>, diary: Option<&str>,
                                terms: &[String], limit: usize)
        -> Result<Vec<Fact>, StorageError>;
    // Today: open_conflicts_matching_terms(kb, terms, limit) — sqlite.rs:4467.
    pub fn open_conflicts_matching_terms(&self, kb: Option<&str>, diary: Option<&str>,
                                         terms: &[String], limit: usize)
        -> Result<Vec<(FactConflictRow, Fact, Fact)>, StorageError>;
    pub fn count_open_conflicts_matching_terms(&self, kb: Option<&str>, diary: Option<&str>,
                                               terms: &[String]) -> Result<i64, StorageError>;

    // SR-3 — the history view stops ignoring its scopes (deliverable 5 + 2).
    // Today: list_recent_conflicts(limit) — sqlite.rs:4761.
    pub fn list_recent_conflicts(&self, diary: Option<&str>, kb: Option<&str>,
                                 limit: usize, offset: usize)
        -> Result<Vec<(Fact, Fact)>, StorageError>;
    pub fn count_recent_conflicts(&self, diary: Option<&str>, kb: Option<&str>)
        -> Result<i64, StorageError>;

    // SR-4 — conflict keys reach the read surface (deliverable 4).
    // `FactConflictRow` (sqlite.rs:346-360) gains, all Option for pre-migration rows:
    //   pub decision_key: Option<String>,
    //   pub member_key: Option<String>,
    //   pub registry_version: Option<String>,

    // SR-5 — the resolve race (brief move 4; facts.rs:451-506).
    // NO NEW API. storage-journal §2.11 already ships the atomic claim+decision:
    //   resolve_conflict_with_decision(conflict_id, kb, diary, key_encoded,
    //       verdict, authority, actor, note, key_version)
    //       -> Result<Option<Uuid>, StorageError>
    // This lane calls exactly that (§3.4.8). SR-5 is only a request that storage
    // KEEP it, and that `Ok(None)` be the sole lost-claim signal — a second
    // `claim_*` entry point would reopen the race at the seam between calls.

    // SR-10 — composition, so the effect cannot commit apart from the claim.
    // Storage must confirm that `close_fact_interval` / `supersede_fact_with`
    // may be called by the CLI inside the same `with_tx` that wraps
    // `resolve_conflict_with_decision`, and that a rollback there also rolls
    // back the queue-row claim and the decision row. If storage would rather own
    // the whole composite, it takes an `effect` argument and this lane stops
    // calling `supersede_fact_with` directly. Either answer is fine; the
    // unstated version is not, because it leaves a resolved queue row with no
    // closed interval representable.

    // SR-6 — index so `total` stays O(index):
    //   CREATE INDEX IF NOT EXISTS idx_facts_scope_open
    //     ON facts(kb, diary, valid_until, superseded_by);

    // SR-7 — honest totals for the fact listings (deliverable 2).
    pub fn count_facts_scoped(&self, diary: Option<&str>, kb: Option<&str>,
                              include_superseded: bool) -> Result<i64, StorageError>;
    pub fn count_facts_by_subject_scoped(&self, subject_norm: &str, diary: Option<&str>,
                                         kb: Option<&str>, include_superseded: bool)
        -> Result<i64, StorageError>;
    // and `offset: usize` added to `list_facts_scoped` (sqlite.rs:4646) and
    // `list_open_fact_conflicts_with_facts` (sqlite.rs:4320).
    // `list_facts_by_subject_scoped` (facts.rs:63-72's query) needs `limit` AND
    // `offset` — it is unbounded today, and an offset without a limit is not a
    // page (§3.4.2).

    // SR-8 — CW-4: `supersede_fact_with` must take the authority from its
    // `resolution` argument (`"caller"` from facts.rs:480/492/630) and must NOT
    // hard-code `authority = "heuristic"`, or every caller decision is audited
    // as a machine close and CM-2's audit query reports the opposite of the truth.

    // SR-9 — totals for the remaining listings (deliverable 2).
    pub fn count_entities(&self, kb: Option<&str>, min_mentions: u32) -> Result<i64, StorageError>;
    pub fn count_entities_by_kind(&self, kb: Option<&str>, kind: &str, min_mentions: u32)
        -> Result<i64, StorageError>;
    pub fn count_documents(&self, kb: Option<&str>) -> Result<i64, StorageError>;
    pub fn count_memories_scoped(&self, diary: Option<&str>, kb: Option<&str>)
        -> Result<i64, StorageError>;
    // plus `offset: usize` on list_entities, list_entities_by_kind,
    // list_documents, list_memories.
}
```

### 6.3 Requests to fact-semantics (`engram-graph`, `fact_write.rs`)

- **FS-1**: `engram_graph::semantics::safety_gate(verdict, &new_view, &old_view)` must be `pub`
  and callable from `commands/facts.rs::triage` (spec-fact-semantics §6.4.1 already promises it).
- **FS-2**: `SemanticKey::for_pair` + `encode()` and `SEMANTIC_KEY_VERSION` must be `pub` so
  `facts resolve` can key its `decision.recorded` (§3.4.8).
- **FS-3**: `allowed_actions(verdict, new, old) -> &'static [&'static str]` must be `pub`; this
  lane passes its output into `respond.rs` per R12 (respond.rs itself must not depend on the
  classifier).
- **FS-4**: `FactView` construction from a `&Fact` must be `pub` (this lane holds `Fact` rows, not
  views).

### 6.4 Requests to contract-codegen

- **CC-R1** — *withdrawn, nothing to build.* An earlier draft asked for a new
  `respond::resolve_payload`. `spec-contract-codegen.md` §2.4 already ships
  `resolve_conflict(conflict_id, allowed_actions) -> ResolvePayload` and
  `attach(&mut obj, &payload)`, which is R12 satisfied. This lane calls those from
  `facts.rs::conflict_entry`, `facts.rs::resolve` and `recall.rs::surface_conflicts`. Recorded as
  withdrawn rather than deleted so nobody re-derives the divergent name.
- **CC-R2**: `HandleKind::prefix()`/`Handle::to_string()` must render `mem|chunk|fact|conflict|
  entity|doc` so this lane never hand-builds a prefix string.
- **CC-R2b**: a decision on whether `HandleKind` gains a `wiki` variant (§3.8). It has none today,
  and a wiki page is not resolvable by any mutator. Either add it and make `resolve_handle` accept
  it, or rule that wiki pages carry `page_id` and no handle. This lane will not invent a prefix
  that fails resolution.
- **CC-R3**: the generated contract test must accept the §4.1 additive field list.
- **CC-R4**: `output::Listing::total` must carry a SQL `COUNT(*)` — `i64`, or `usize` with a
  documented cast — and gain a constructor that takes this lane's cursor string (§2.1).
- **CC-R5**: `output::Scope::chunks_considered` becomes `Option<usize>`, key omitted when `None`,
  so non-retrieval listings do not report a fabricated `0` chunks scored (§2.2). Plus an additive
  `origin: &'static str` on the scope object, vocabulary `flag|config|all_kbs`.
- **CC-R6**: `output::Absence` gains `reason: &'static str` (vocabulary per §2.3.2) and relaxes
  `threshold` to `Option<f32>`. If `threshold` stays `f32`, this lane omits the field entirely
  rather than emit a fabricated `0.0` (§2.3.1). **This is a lead ruling, not a preference** — the
  two specs currently disagree and `contract_envelope` will fail on whichever spelling loses.
- **CC-R7**: `close-old` must enter the `resolve_action` vocabulary (contract-codegen's vocabulary
  table lists only `keep-both|supersede-old|reject-new`) before this lane emits it in
  `allowed_actions` or accepts it on `--action`, or
  `allowed_actions_are_from_the_vocabulary` fails. If the lead refuses the widening, §3.4.8's
  `close-old`/`--as-of` half is dropped from wave 3 and the rest of `resolve()` is unaffected.

### 6.5 Requests to write-surfaces

- **WS-1**: `commands/memory.rs::list()` — which write-surfaces already owns and is rewriting
  (spec-write-surfaces §3.1) — emits `output::Listing` with `total` from `count_memories_scoped`
  (SR-9) and `cursor` from `read_surface::next_cursor`, alongside the per-row `mem:<uuid>` handle
  it already plans. Without it `memory list` is the one listing surface that still means "page"
  by "count" (§1.3, §3.9).
- **WS-2**: `forget` and `edit` must accept a bare `memory_id` and a kind-prefixed handle
  (interfaces.md §1). Deliverable 1 is only *provably* closed by §5.1.2/§5.1.3, which exercise
  those mutators; until WS-2 lands those tests are `#[ignore]`d and the brief's headline defect
  is unproven end to end.

---

## 7. Risks and open questions

### 7.1 Risks

**R-1 — this lane is downstream of three others.** Nothing in deliverables 4, 7 or 8 compiles
until `engram_core::{interval, relations}` (storage-journal, wave 2), `respond.rs`
(contract-codegen, wave 2) and `safety_gate`/`SemanticKey` (fact-semantics, wave 3) exist.
*Mitigation*: build in dependency order — deliverables 1, 2, 3 and 6 (provenance, paging, absence,
search parity) touch no other lane's types and land first; 4, 7, 8 land against interfaces.md
stubs and are gated by the lead's wave-3 sweep gate. If wave 2 slips, the first four still ship.

**The mitigation is narrower than it looks, and the honest version is this**: deliverables 1, 2
and 5 are downstream of the §6.2 storage requests, which storage-journal has not yet accepted
(see §6.2's acceptance gate). Deliverable 4's `fact_to_json` fields (`closed_by`, `close_reason`,
`close_authority`) do not compile until storage-journal's D10 lands them on `engram_core::types::
Fact` — `types.rs:151-191` has none of them today, so §4.2's "serde default on a v9 row" is a
property of a struct that does not exist yet. `is_open_fact` (`interval.rs:86-88`) and
`engram_core::relations` HAVE landed. So the truly unblocked set on day one is: the cursor codec,
`RecallStats`, provenance for `chunk`/`claim`, the absence certificate, and the search-parity
refactor. Everything else waits on a named acceptance.

**R-2 — the `total` counts are extra queries on every listing.** A second `COUNT(*)` per response.
*Mitigation*: SR-6's index; the counts are index-only on the hot paths (`facts list`,
`facts conflicts`), and `count_open_fact_conflicts` already runs on the `conflicts` path today
without complaint. Measure with `bench personal` before/after; if a count exceeds ~50 ms on the
real-shaped store, degrade that surface to `total: null` plus `total_reason: "too_expensive"`
rather than shipping a wrong number.

**R-3 — offset cursors skew under concurrent writes.** A row inserted between pages can be missed
or repeated. *Mitigation*: honest documentation (the cursor is opaque and the listing is not a
snapshot), stable secondary sort by `id`, and the fingerprint check that at least prevents paging
a *different* query. Keyset cursors are the right answer and are deferred: they need per-surface
sort-key support in storage, which is a wave-4 conversation. Recorded here so the deferral is a
decision, not an oversight.

**R-4 — `verdict` and `status` could drift.** Two fields answering one question is how
`count`-means-two-things happened. *Mitigation*: they are computed from the SAME boolean in one
place (§3.3.3), and §5.3 pins the correspondence.

**R-5 — R7's read-side guard could mask a fact-semantics bug.** If this lane rewrites
`supersede-old` → `requires_context` for safety pairs on read, a write-side regression that keeps
emitting `supersede-old` becomes invisible. *Mitigation*: when the guard fires on a row whose
stored `suggested` disagrees, emit `metadata.suggested_action_overridden: <n>` so the mismatch is
counted and visible, and fact-semantics' own tests keep asserting the write-side value directly.

**R-6 — behaviour changes in §4.3 are user-visible.** Especially item 2 (diary-scoped conflicts),
which narrows output. *Mitigation*: `metadata.scope` explains every narrowing; CLP-6 puts all four
in the CHANGELOG.

**R-7 — `memory search`/`recall` unification could change defaults by accident.** *Mitigation*:
§3.10 preserves every current default explicitly; §5.7's parity test asserts key sets, not values,
so an accidental default change shows up as a behaviour diff in the existing `tests/cli.rs` suite
rather than being hidden by the parity test.

### 7.2 Open questions for the lead

- **OQ-1 — `total` semantics on unbounded queries.** `facts show` and `wiki` list everything;
  `total == returned` there. Confirm that is acceptable rather than omitting the field. *Proposed:
  emit it; a caller should not have to know which surfaces are bounded.*
- **OQ-2 — `research`'s KB default.** `recall` defaults to every KB (via `resolve_kb_scope`);
  `research` defaults to `--kb`. Unify, or document the difference? *Proposed: document in wave 3,
  unify in wave 4 with a CHANGELOG entry, because widening a default scope silently changes cost.*
- **OQ-3 — conflicts counts shape on `recall`.** `data.conflicts_returned` /
  `data.conflicts_total` beside the existing array (proposed, strictly additive), versus wrapping
  the array in an object (breaking). *Proposed: the additive form; §3.3.6 already assumes it.*
- **OQ-4 — should `memory search` and `recall` share defaults?** They differ on `--mode`
  (`agent` vs `evidence`). Wave 3 preserves both. *Proposed: unify in wave 4 under contract-codegen
  so one schema generates both.*
- **OQ-5 — RESOLVED. `commands/memory.rs` is write-surfaces'.** The brief's ownership table lists
  it under write-surfaces and spec-write-surfaces §3.1 already rewrites it. This lane's paging need
  is request WS-1 (§6.5). Kept in the list so the earlier ambiguity is visibly closed.
- **OQ-6 — who un-`#[ignore]`s §5.6.** CD-2/CC-4 name a lead-owned wave-3 sweep gate spanning
  storage-journal and read-surfaces; this spec assumes the lead runs it and flips both this lane's
  §5.6 tests and fact-semantics' §5.42 tests together.
- **OQ-7 — `absence.threshold` may stay permanently `null`.** There is no score cutoff anywhere in
  `hybrid_recall` today. If the interface requires a non-null threshold, someone must first
  introduce a real cutoff; this lane will not fabricate one (§2.3.1). *Superseded by OQ-10, which
  states the same problem as the two-spec type conflict it actually is.*
- **OQ-8 — who adds `base64.workspace = true` to `crates/engram-cli/Cargo.toml`** (CLP-9, §2.1).
  The crate is already a workspace dependency, so nothing new enters the tree; only the manifest
  edit needs an owner.
- **OQ-9 — `total` on ranked reads** (§2.8). Interfaces v2 pins `total` as "matching rows in
  scope"; §2.8 sets it to `candidates_fused` on `recall`/`research`/`memory search`. Every value is
  honest, but one field then means two things depending on the surface, which is the count-drift
  class constraint 3 exists to prevent. *Proposed: emit `data.candidates_fused` (matching
  `metadata.candidates_considered`, `recall.rs:239`) and leave `total` off ranked surfaces
  entirely, so `total` keeps exactly one meaning everywhere it appears.* Blocks §3.3.5, §3.5 and
  §5.2.7.
- **OQ-10 — `absence.threshold` and `absence.reason` are a two-spec disagreement, not a
  preference** (CC-R6, §2.3.1). Contract-codegen pins `threshold: f32` with no `reason`; this lane
  needs `Option<f32>` and a `reason`. Whichever way it is ruled, one of the two specs is edited
  before wave 2 ships `output.rs`. Supersedes the narrower OQ-7.
- **OQ-11 — stopped-without-successor facts are still invisible to `facts conflicts` (history
  view).** storage-journal §2.9 deliberately keeps `list_recent_conflicts` on
  `superseded_by IS NOT NULL`, and SR-3 only adds the diary/kb filters. So after the redesign a
  medication that was *stopped* (closed with no successor) appears in `facts show` under
  `historic` and in `recall`'s conflicts array as `status: "ended"`, but never in the history
  view. *Proposed: intended for wave 3 — widening that query would flood a review surface with
  non-adjudicable rows — but say so in agent-info rather than leaving it as an accident.* Related:
  §3.3.6 does add `ended` rows to `recall`'s conflicts array, which changes what an agent treats
  as actionable there; if the lead wants those excluded, the filter is one line at `recall.rs:310`
  and belongs in the same ruling.

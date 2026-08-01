# coherence report — arch/v0.4 cross-lane judging

Judged 2026-08-01 22:52 against `docs/arch/redesign-brief.md`, `docs/arch/interfaces.md`, the
shipped tree at 0.3.1 plus the lead's wave-1 pre-landings, and the lane specs present in
`docs/arch/`.

## 0. What was judged, and what was not

One of seven lane specs existed at judging time:

| lane | spec | status |
|---|---|---|
| fact-semantics | `spec-fact-semantics.md` (90 KB, revised post-Grok-review) | judged |
| storage-journal | — | absent |
| write-surfaces | — | absent |
| read-surfaces | — | absent |
| contract-codegen | — | absent |
| daemon-jobs | — | absent |
| reasoning-layer | — | absent |

Cross-lane judging is still possible and still finds real defects, because three of the lanes
have already published binding artefacts other than a spec:

- **storage-journal has already landed code.** `crates/engram-core/src/handle.rs`,
  `crates/engram-core/src/events.rs`, `crates/engram-storage/src/paths.rs` and
  `crates/engram-cli/src/lib.rs` all carry a 2026-08-01 21:57–21:58 mtime, the same minute as
  `interfaces.md`. `Handle`, `HandleKind`, `HandleParseError`, the fifteen-variant `EventKind`,
  the `ENGRAM_STORE` override and the agent-info-before-`AppContext` fix are on disk. Anything
  another spec proposes to build there is duplicate work, and anything that disagrees with them
  is already a mismatch.
- **interfaces.md is the contract-codegen and storage-journal surface** — envelope fields, error
  codes, handle grammar, event payloads.
- **the brief's lane table is itself an ownership specification**, and one spec already
  contradicts it.

Findings below carry stable ids. Sections 1–4 are ordered most severe first within each section.
Section 5 is the consolidated, deduplicated list of what `interfaces.md` must satisfy.

Six lanes are unjudged. Every finding in sections 1–4 that names an absent lane is a prediction
from the contract, not an observation of that lane's spec; re-run this judging when the six land.

---

## 1. Interface mismatches

### CM-1 — the arity registry has two possible homes, and two homes means two registries

`interfaces.md` §3 states:

```rust
pub enum Arity { SetValued { safety: bool }, SingleValued { safety: bool } }
pub fn relation_arity(canonical: &str) -> Arity;
pub fn member_identity(relation: &str, object_norm: &str) -> String;
```

with no crate comment. Every other Rust block in `interfaces.md` names its crate — §1 says
`// engram-core`, §2 says `// engram-storage`, §6 says `// engram-cli (core module)`. §3 says
nothing, and it sits under a heading whose other half (the `facts` table DDL, `closed_by`,
`close_reason`) is unambiguously storage-journal's.

`spec-fact-semantics.md` §2.1 puts `relation_spec`, `relation_arity`, `member_identity`,
`canonical_relation`, `cessation_of`, `is_safety_slot` and `predicate_arity` in
`crates/engram-graph/src/relations.rs`, and re-exports only the `Arity` enum from `engram-core`.

Storage-journal reading §3 in its own voice will implement `relation_arity` and
`member_identity` in `engram-core` next to the enum — it needs `member_identity` anyway, because
the v9→v10 migration keys 14.5k decision rows by `SemanticKey::for_pair`, and `SemanticKey`
depends on the member heuristic. `engram-core` cannot depend on `engram-graph`, so the migration
cannot call the fact-semantics copy. The result is two arity tables and two member heuristics
that agree on the day they are written and drift on the first registry change — and the arity
table IS the polypharmacy fix. A store migrated by one registry and then written by another
re-opens the storm it was migrated to close.

**Pin**: one home, one `REGISTRY_VERSION`, one `MEMBER_IDENTITY_VERSION`. If the migration needs
it, the registry belongs in `engram-core` and `engram-graph::relations` re-exports — which is
also what `spec-fact-semantics.md` §7 risk 6 asks for on separate grounds (making the safety gate
structural rather than a discipline).

### CM-2 — `facts.close_authority` is a projection column no event carries, so the journal cannot rebuild it

`spec-fact-semantics.md` §2.6 adds `ALTER TABLE facts ADD COLUMN close_authority TEXT`, §6.6.1
asks the lead to record it, and §4.1 Phase 3 makes it load-bearing: "no fact acquires
`close_authority = 'heuristic'` on a safety relation (a single SQL assertion, and the reason
`close_authority` is worth a column)". The safety gate's only after-the-fact audit is this column.

`interfaces.md` §2 defines `fact.interval_closed (fact_id, valid_until, closed_by_fact?, reason)`.
There is no authority field. §6.6 asks for `authority` on `decision.recorded` and does **not** ask
for it on `fact.interval_closed`.

`interfaces.md` §2 invariant 3 requires `engram admin rebuild` to "reproduce byte-identical
projection state (property-tested)". A rebuild replays `fact.interval_closed` and has nothing to
write into `close_authority`. Every closed interval comes back NULL, the property test fails, and
the one query that proves no heuristic ever closed an allergy returns nothing.

The same hole applies to `PlannedClose.resolution_tag` and to the `superseded_by` compatibility
write of `interfaces.md` §3: the event payload carries `closed_by_fact?` but the projection
column is `closed_by`, and nothing in the payload records whether `superseded_by` was also set.

**Pin**: `fact.interval_closed (fact_id, valid_until, closed_by?, reason, authority, sets_superseded_by)`
— or drop the column and accept that the safety audit is untestable.

### CM-3 — interval types are assigned to storage-journal by the brief and to fact-semantics by the spec

The brief's lane table:

> | storage-journal | crates/engram-storage/src/** (journal.rs NEW, sqlite.rs, paths.rs, migration), **crates/engram-core/src/** (event/interval/handle types)** |

`spec-fact-semantics.md` §2.2 creates `crates/engram-graph/src/interval.rs` holding `Interval`,
`is_open_fact`, `CloseReason`, `CloseAuthority`, `CloseVerdict`, `may_close`, `close_at`.

Two lanes have been told to own the interval types, in two crates. It is not academic: the
migration (storage-journal) applies `close_at`'s clamp — `spec-fact-semantics.md` §4.1 step 2
says so explicitly, "the `MAX` is `close_at`'s clamp (§2.2) expressed in SQL, and it is not
optional" — and `engram-storage` cannot depend on `engram-graph`. So the clamp is written twice,
once in Rust and once in SQL, in two lanes, and the spec's own §2.2 says "the clamp is not
optional in one place and advisory in the other" while creating exactly that arrangement.

`reasoning-layer` compounds it: `engram state` consumes `open_intervals()` and `relation_spec()`
(spec §1.2), so `commands/state.rs` would depend on `engram-graph` for interval semantics and on
`engram-storage` for the rows.

**Pin**: interval types in `engram-core` per the brief, with `may_close` and the registry beside
them; `engram-graph::interval` becomes a re-export or disappears.

### CM-4 — `Arity` names two different types in one workspace, and read-surfaces imports the wrong one

Today `crates/engram-cli/src/commands/facts.rs:20-24` imports `Arity` from `crate::fact_write`
and matches `Arity::Single | Arity::Multi` at `:436-439`.

`spec-fact-semantics.md` §2.1 keeps that compiling by renaming the old enum `LegacyArity` and
re-exporting it from `fact_write.rs` under **both** spellings — `pub use
engram_graph::relations::LegacyArity as Arity;` plus `pub use ...::LegacyArity;` — while the new
two-variant-with-payload `Arity` lives in `engram-core` and is deliberately **not** re-exported
from `fact_write.rs`.

So after wave 3 the workspace has `engram_core::…::Arity { SetValued, SingleValued }` and
`engram_cli::fact_write::Arity { Single, Multi }`, both public, both named `Arity`. The moment
read-surfaces (which owns `facts.rs` and must edit it anyway — see CW-2) adds
`use engram_core::…::Arity` for the new `arity_class` field that spec §6.4.3 asks it to emit, the
file has two `Arity` in scope and does not compile. The spec's claim that "no read-surfaces file
sees two meanings of one name" holds only while read-surfaces never touches its own file.

**Pin**: the legacy projection keeps a legacy name at every call site. Rename the JSON-shaped
helper to `predicate_arity() -> LegacyArity` everywhere, including in `facts.rs`, and let `Arity`
mean exactly one thing.

### CM-5 — `decision.recorded` has no `authority` and no `kb`, and both decide whether a decision replays

`interfaces.md` §2: `decision.recorded (decision_id, semantic_key, verdict, conflict_id?, note?, actor)`.

`spec-fact-semantics.md` §2.3.2 makes `DecisionAuthority::{Caller, Machine}` the discriminator
for the entire replay safety rule: a caller decision is binding in every direction, a machine
decision is binding only when it is `KeepBoth`, "because replaying a machine `CloseOld` would
re-freeze the exact classifier bug this redesign exists to remove". §6.6.3 asks for the field and
gives the reason: `actor` is free text (`"cli:<diary>"`, `"job:<uuid>"`), and deriving a safety
property from a string convention is how the property gets lost.

Three further gaps in the same payload:

- **`kb`.** §6.9 asks that the decision LOOKUP be scoped by `kb` and that diary deliberately not
  be. The event row has a `kb` column, but the payload does not, so a rebuild that reconstructs
  `fact_decisions` from the journal has to trust the column. Worth stating; the column is enough
  if it is stated.
- **the verdict vocabulary.** `interfaces.md` never enumerates it. The spec uses
  `keep_both | close_old | reject_new | void`, where `void` is new and is a terminal state the
  migration needs (§4.1 steps 4 and 5.2). Contract-codegen will generate a JSON Schema from
  whatever `interfaces.md` says; today it would say nothing.
- **`key_version`.** The spec pins `SEMANTIC_KEY_VERSION = "semantic_key@1"` on every row and
  event so a key change can be re-keyed rather than re-litigated. Not in the payload.

### CM-6 — `lookup_decision` / `binding_decision`, and `DecisionRow` crosses a lane boundary undefined

`spec-fact-semantics.md` §2.3.2 defines the trait method as `binding_decision(&self, kb, &SemanticKey)
-> Result<Option<DecisionRecord>, StorageError>` precisely to avoid an inherent method shadowing a
trait method, and §6.2 asks storage-journal for the inherent
`lookup_decision(&self, kb, key_encoded: &str) -> Result<Option<DecisionRow>, StorageError>`.

`DecisionRow` is named only in a fact-semantics document and must be defined by storage-journal.
`interfaces.md` mentions neither method nor type. Two lanes, one type, no contract — and the trait
`DecisionLedger` is implemented for a foreign type (`SqliteStore`) inside `engram-graph`, so a
change to `DecisionRow` breaks a lane that does not own the file where the break appears.

### CM-7 — `insert_fact_conflict` returning `Ok(false)` is ambiguous, and the spec's recovery reads it as unambiguous

`spec-fact-semantics.md` §2.4 turns an `Ok(false)` from `insert_fact_conflict` into a real
response: "look up the open row for `(kb, decision_key)` and push a `queued` entry naming THAT
`conflict_id`, with `"deduplicated": true`".

`crates/engram-storage/src/sqlite.rs:1106` already declares `UNIQUE(new_fact_id, old_fact_id)` on
`fact_conflict_queue`, and the insert at `:4240` is `INSERT OR IGNORE`. After
`idx_fcq_open_key` lands there are **two** distinct reasons for `Ok(false)`: the same fact pair is
already queued (old constraint), or a different pair shares the semantic key (new constraint).
The spec's recovery is correct only for the second. For the first it looks up a row by
`decision_key` and may return a conflict id for a different pair, or find nothing at all when the
existing row predates the backfill and has `decision_key = NULL` — an exit-0 success reporting a
conflict id that does not answer the caller's question.

**Pin**: `insert_fact_conflict` returns which constraint fired, or returns the surviving row id.

### CM-8 — `suggested_action` loses `supersede-old` globally in one spec and only for safety in the contract

`interfaces.md` §3 constrains one case: "if safety, open a conflict whose `suggested_action` is
`requires_context` (NEVER `supersede-old`)". The brief's move 2 kills `fact_write.rs:1209-1213`,
which is the `detector == "slot"` suggestion.

`spec-fact-semantics.md` §1.4 removes `supersede-old` from the suggestion vocabulary for **every**
pair, safety or not, and §7 risk 9 raises it as an open question for the lead without waiting for
an answer.

Until the lead rules, three lanes document three vocabularies: fact-semantics emits
`keep-both | requires_context`, `interfaces.md` implies `keep-both | supersede-old | requires_context`,
and contract-codegen's `agent_info.rs:41` currently promises agents that under `--autonomy
assisted` they should "run the included resolve_command". A generated JSON Schema with an
`enum` for `suggested_action` will be wrong for whichever lane lands second.

### CM-9 — `resolve_command` / `resolve {}` has four producers in four lanes and no shared builder

`interfaces.md` §4 replaces the `resolve_command` fields with
`resolve { program: "engram", args: [...], allowed_actions: [...] }` and keeps the old string
carrying one pipe-free command. §6 says "CLI and daemon share one response-builder module per
command family; serve.rs may not construct bespoke response JSON" — and assigns that module to
daemon-jobs, in wave 4.

The producers today:

| site | lane |
|---|---|
| `crates/engram-cli/src/fact_write.rs:1232` | fact-semantics (wave 3) |
| `crates/engram-cli/src/commands/facts.rs:371` | read-surfaces (wave 3) |
| `crates/engram-cli/src/commands/recall.rs:345` | read-surfaces (wave 3) |
| `crates/engram-cli/src/commands/serve.rs` (bespoke JSON, `:577-610`) | daemon-jobs (wave 4) |
| `crates/engram-cli/src/agent_info.rs:41,43,104,169` (documents the field) | contract-codegen (wave 2) |

Three lanes ship three implementations of one field in wave 3, and wave 4 rewrites them. The
`allowed_actions` list is worse: `spec-fact-semantics.md` §2.3.1 defines it as
`allowed_actions(v, new, old) -> &'static [&'static str]` in `engram-graph::semantics`, so the
daemon's builder in wave 4 will need to depend on the fact-semantics classifier to render a
recall response.

**Pin**: the response-builder module lands in wave 2 as part of contract-codegen, not wave 4.

### CM-10 — `ErrorCode` does not exist, and `NotFound` currently exits 1

`interfaces.md` §4 pins
`ErrorCode { BadInput, NotFound, WrongIdKind, Config, Transient, RateLimited, SqliteBusy, ConflictAlreadyResolved, MalformedJson }`
with `NotFound → 3`.

The shipped `crates/engram-cli/src/error.rs` has none of `NotFound`, `WrongIdKind`, `SqliteBusy`,
`ConflictAlreadyResolved`, `MalformedJson`, and maps `CliError::Storage(_)` — which is where
`StorageError::NotFound` arrives — to exit **1**. `interfaces.md` never says how `StorageError`
variants map onto `ErrorCode`, and `error.rs` is contract-codegen's while `StorageError` is
storage-journal's.

Every mutator that follows `interfaces.md` §1 ("Unknown → `not_found`, exit 3 — on every mutator
including `forget`") by returning a storage error gets exit 1 instead, and the contract test that
was supposed to catch it lives in the lane that owns the mapping.

### CM-11 — `member_identity`'s precondition differs between the contract and the spec

`interfaces.md` §3: `pub fn member_identity(relation: &str, object_norm: &str) -> String;`

`spec-fact-semantics.md` §2.1: `pub fn member_identity(canonical_relation: &str, object_norm: &str) -> String;`
and, for the registry lookup beside it, "Input MUST be canonical (`canonical_relation`)".

Same type, different contract. A caller — the migration, `facts triage`, `engram state` — that
passes a raw predicate gets a member identity computed against a relation the registry does not
know, so `allergic_to` and `is_allergic_to` produce different members for the same drug, and two
intervals that should have been one dose-change question become two coexisting facts. No compiler
catches it.

**Pin**: either canonicalize inside the function, or make the parameter a newtype the caller
cannot construct from a raw string.

### CM-12 — module paths for the shared types are unstated, and one spec has already guessed wrong

`interfaces.md` §1 writes `// engram-core` above `HandleKind`/`Handle`; the landed code puts them
in `engram_core::handle`. §3's `Arity` has no home at all, and `spec-fact-semantics.md` §2.1
writes `pub use engram_core::types::Arity;` — a module that exists and does not contain it.
`engram-core`'s convention is one module per concept (`handle.rs`, `events.rs`, `temporal.rs`),
so `types::Arity` is very likely wrong. Six lanes will each guess a path for `Arity`,
`EventKind`, `Handle`, `Resolved` and `ErrorCode`.

**Pin**: fully-qualified paths in `interfaces.md`, and a re-export list in `engram-core/src/lib.rs`
(which today re-exports only `CoreError` and seven `types::*` names — not `Handle` or `EventKind`).

### CM-13 — `ENGRAM_HOME` and `ENGRAM_STORE` are the same variable under two names

Brief: "**store profiles/isolation** (`--store <path>` / `ENGRAM_HOME`, ephemeral test stores)".
`interfaces.md` §5 and the shipped `crates/engram-storage/src/paths.rs`: `ENGRAM_STORE`.

`ENGRAM_STORE` is right — it is in the code, with a header comment explaining why it exists. The
brief's name will nonetheless reach a lane spec, a test helper, `agent-info` and `skill.md`,
because five of the seven lanes read the brief before they read `paths.rs`.

---

## 2. Unstated cross-lane dependencies, and what they do to the sequencing

### CD-1 — `ExtractedFact` gains fields in wave 3 and breaks a wave-4 file

`spec-fact-semantics.md` §2.5 adds `valid_until` and `state` to `ExtractedFact` with
`#[serde(default)]` and a `Default` impl. `crates/engram-cli/src/commands/compile.rs:866-875`
constructs the struct literally, with no `..Default::default()`. `compile.rs` is daemon-jobs,
wave 4.

The moment fact-semantics lands, `cargo build --workspace` fails in a file its lane may not touch
(brief constraint 7) and whose lane is not scheduled for another wave. The spec pre-negotiates
the one-line change in §6.5 — correctly — but the brief has no mechanism for a cross-lane
one-liner, and constraint 5 requires the workspace green *before commit*.

**Sequencing**: either the lead owns a "cross-lane patch" allowance, or `#[non_exhaustive]` +
`Default` is not enough and the struct needs a constructor. This is the pattern, not the
instance: any additive field on a shared struct crosses a wave boundary.

### CD-2 — the `superseded_by IS NULL` sweep is the load-bearing dependency and belongs to no lane

`spec-fact-semantics.md` §7 risk 5 calls this "the highest-risk cross-lane item in the spec". It
is: a fact closed with `close_reason = 'stopped'` gets `valid_until` set and `superseded_by` left
NULL, and every "active" query in the store tests `superseded_by IS NULL`. Until the sweep lands,
a stopped medication reads as current on every surface — which is the failure mode move 2 exists
to prevent, introduced by move 2.

The actual sites, verified:

| file | lines | lane |
|---|---|---|
| `crates/engram-storage/src/sqlite.rs` | 488, 3987, 4016, 4119, 4159, 4212, 4563, 4579, 4615, 4630, 4670, 4685, 4727, 4743, 4796 | storage-journal |
| `crates/engram-cli/src/commands/facts.rs` | 123, 156, 157 | read-surfaces |
| `crates/engram-cli/src/commands/recall.rs` | 310 | read-surfaces |

The spec names `commands/people.rs` and `retrieval.rs` as sites. Neither contains the string
`Fact` or `facts` — `people.rs` has zero fact reads and `retrieval.rs` has one unrelated comment.
So the spec routes the sweep to two files that have no such code and omits the two that do, both
in read-surfaces.

**Sequencing**: the sweep is a single named wave-3 integration item spanning storage-journal and
read-surfaces, with an owner. `spec-fact-semantics.md` §5.42 currently marks the test that would
prove it `#[ignore]`.

### CD-3 — decision replay needs three lanes before its own lane can prove it works

`spec-fact-semantics.md` tests 38–39 prove that a caller decision survives re-derivation. They
need, in order: storage-journal's `append_event` + `record_decision` + a non-cascading
`fact_conflict_queue` (wave 2), read-surfaces appending `decision.recorded` from `facts resolve`
(wave 3, requested in §6.4.2 — today `facts.rs:451-517` records nothing beyond
`resolve_fact_conflict`, and `keep-both` writes literally nothing), and write-surfaces' `edit`
no longer calling `delete_facts_for_memory` (wave 3; `edit.rs:190` and `sqlite.rs:1925`, with the
FK cascade at `sqlite.rs:1106-1108`).

Move 3 is therefore not testable inside any single lane, in any single wave. §5.42 marks both
tests `#[ignore]`, which is honest and means the headline guarantee of move 3 ships ungated
unless the lead adds a wave-3 integration gate that owns them.

### CD-4 — one new command needs four lanes, and one of them is nobody

`engram state` (reasoning-layer, wave 5) requires:

| edit | owner |
|---|---|
| `crates/engram-cli/src/commands/state.rs` | reasoning-layer |
| `crates/engram-cli/src/cli.rs` (the clap subcommand) | contract-codegen |
| `crates/engram-cli/src/lib.rs` (dispatch arm) | daemon-jobs |
| `crates/engram-cli/src/commands/mod.rs` (`pub mod state;`) | **no lane** |

The same is true of `engram admin rebuild`, which `interfaces.md` §2 invariant 3 requires and
`spec-fact-semantics.md` §4.1 names as its architectural rollback, and which no lane owns at all.

### CD-5 — the `--store` flag is split across three lanes and two waves

`interfaces.md` §5 pins the precedence `--store <dir>` > `ENGRAM_STORE` > ProjectDirs. The flag
does not exist in `cli.rs` (contract-codegen), `AppContext::new` takes no path
(`crates/engram-cli/src/context.rs`, daemon-jobs, wave 4), and the resolution lives in `paths.rs`
(storage-journal, wave 2). The env half already works; the flag half cannot exist until wave 4
unless `context.rs` moves.

### CD-6 — no test in the repository sets `ENGRAM_STORE`, and the test files belong to no lane

`interfaces.md` §5: "All tests set `ENGRAM_STORE` to a TempDir (never rely on HOME tricks again)."
`grep -rl ENGRAM_STORE crates/engram-cli/tests/` returns nothing. The suite isolates through
`point_at_home` (`tests/cli.rs:15-20`), which sets `HOME` and the three XDG variables and not
`ENGRAM_STORE`.

`tests/cli.rs` is 5,785 lines and belongs to no lane — the ownership table lists only
`tests/contract_*.rs` (contract-codegen), `tests/daemon_*.rs` (daemon-jobs) and `tests/state_*.rs`
(reasoning-layer). `wave0_*.rs` and `wave05_*.rs` are likewise unowned, and
`spec-fact-semantics.md` §5.41 requires edits to two of them
(`cli.rs:1791`, `wave05_compiled_facts.rs:170`).

### CD-7 — the v10 backup cannot be written where the migration lives

`SqliteStore::open()` calls `migrate()` (`sqlite.rs:812`, `:823`), and `migrate()` runs every step
inside one `with_tx`. A backup file must be written before that transaction opens, only when the
stored version is below 10, and never for `open_in_memory()` (`sqlite.rs:816-824`, which also
migrates). There is no backup mechanism anywhere in the tree today — see CC-3.

### CD-8 — the blake3 decision is one-way and belongs to no lane

`interfaces.md` §3 pins `object_pair_hash = hex(blake3(...))`. `blake3` is not in
`[workspace.dependencies]`, and the workspace `Cargo.toml` is owned by no lane.
`spec-fact-semantics.md` §6.1 offers `Uuid::new_v5` as a dependency-free alternative and notes
the choice "is baked into 14.5k migrated decision keys" — so it must be made once, before the
migration is written, by the lead.

### CD-9 — export v3 splits its format from its command

`interfaces.md` §2 invariant 4 makes export v3 a journal slice and import v3 a replay.
`commands/export.rs` (53 lines, `"version": 2` at `:28`) and `commands/import.rs` are
write-surfaces, wave 3; the slice format and the replay are storage-journal, wave 2. No lane owns
the v2-import compatibility path that "synthesizes genesis events".

### CD-10 — `Fact`'s new fields appear in wave 2 and mean nothing until wave 3

`spec-fact-semantics.md` §6.3 asks `engram_core::types::Fact` to gain `close_reason`,
`close_authority`, `closed_by`. `Fact` is serialized to JSON by read-surfaces
(`facts.rs:1105` `fact_to_json`). The field appears in every `facts list` response one wave before
anything populates it, and `agent-info`/`skill.md` (contract-codegen) must describe it in the
same wave or the generated contract test fails on an undocumented field.

---

## 3. Work assigned to two lanes

### CW-1 — the v9→v10 migration body

The brief gives "migration" to storage-journal. `spec-fact-semantics.md` §4.1 Phase 1 writes it:
six ordered steps, the `MAX(valid_from, COALESCE(...))` clamp in SQL, the `resolution` →
`close_reason` mapping table, the genesis event order, the resolved-row verdict inference, the
open-row key backfill, the duplicate-key collapse with its survivor rule (lowest `created_at`,
ties by `id`), and the rule that `CREATE UNIQUE INDEX idx_fcq_open_key` must come last.

This is correct as an interface request and dangerous as a lane boundary: it is complete enough
to implement, in a document the implementing lane does not own, containing the one algorithm
(duplicate-key collapse) whose absence aborts the migration on any store with a backlog.

**Resolution**: move §4.1 Phase 1 verbatim into `interfaces.md` §2 as the migration contract, so
storage-journal implements a contract rather than another lane's spec.

### CW-2 — `commands/facts.rs`

read-surfaces owns the file. `spec-fact-semantics.md` prescribes six changes to it:
replace the inline safety defence at `:581-593` with `safety_gate` (§6.4.1); append
`decision.recorded` from `resolve` (§6.4.2); add `arity_class` and `safety` to `group_conflicts`
(§6.4.3); make `resolve_command` pipe-free and add `resolve.allowed_actions` (§6.4.4); optionally
add `--action close-old --as-of <date>` (§6.4.5); and keep `:20-24`'s import block untouched
(§2.3) — which contradicts all five of the others.

### CW-3 — the decision ledger

`fact_decisions` DDL, `record_decision`, `lookup_decision` and `DecisionRow` are storage-journal's
(`engram-storage`). `DecisionLedger`, `DecisionRecord`, `binding_decision`, the precedence rule
("newest caller row, else newest machine row, never `void`") and `replay` are fact-semantics'
(`engram-graph`), including `impl DecisionLedger for SqliteStore` written in `engram-graph`
against a foreign type. The index that makes the precedence rule one scan
(`idx_fact_decisions_key ... (kb, decision_key, authority, decided_at DESC)`) is specified by
fact-semantics and created by storage-journal.

One ledger, two lanes, and the safety-relevant half (which decision wins) is in the crate that
does not own the query.

### CW-4 — `supersede_fact_with`

`interfaces.md` §3 keeps `superseded_by`/`superseded_at` populated for `close_reason='superseded'`
as a compatibility view — storage-journal. `spec-fact-semantics.md` §6.2 dictates the body: "kept,
and re-expressed as `close_fact_interval(reason="superseded", authority="heuristic",
closed_by=new_id)`". But `facts.rs:630` calls it from `facts resolve` where the authority is
`caller`, not `heuristic` — so the re-expression as specified would stamp every caller decision
as a heuristic close and make CM-2's audit query report the opposite of the truth.

### CW-5 — `resolve_command` / `resolve {}`

Four producers, four lanes. See CM-9.

### CW-6 — the arity registry and its legacy shims

See CM-1. Additionally `is_safety_slot` and `predicate_arity` are re-exported by fact-semantics
from `fact_write.rs`, consumed by read-surfaces at `facts.rs:436-440`, and documented by
contract-codegen at `agent_info.rs:41` ("Safety slots (allergic_to, takes_medication,
diagnosed_with) are never auto-superseded") — a sentence that stays true in substance and false
in vocabulary once those three relations stop being slots.

### CW-7 — the `ProfileSlot` → `assertion` change and its documentation

`spec-fact-semantics.md` §4.2 changes `fact_type` for new safety facts. The change is
fact-semantics'; the published field is read-surfaces'; the CHANGELOG line and the agent-facing
description are contract-codegen's. The spec asks for the CHANGELOG entry without naming an owner.

---

## 4. Contradictions with the brief's hard constraints

### CC-1 — constraint 1 and `interfaces.md` §5 mandate opposite test-isolation mechanisms

Brief, hard constraint 1: "Every test and every manual binary run MUST use an isolated HOME
(`TempDir` + `.env("HOME", …)`; see the `cmd_in(&tmp)` pattern in crates/engram-cli/tests/cli.rs).
A test that opens the real store is an automatic reject".

`interfaces.md` §5: "All tests set `ENGRAM_STORE` to a TempDir (never rely on HOME tricks again)."

Both documents are declared ground truth; one says use HOME, the other says never use HOME. The
shipped `paths.rs` honours `ENGRAM_STORE` first and falls back to `ProjectDirs`, so HOME still
isolates on macOS — but only because `ProjectDirs` reads `$HOME`, and only if every XDG variable
is set too, which is why `point_at_home` sets four variables to do one job.

`spec-fact-semantics.md` §5 sets both and §4.1 Phase 0 explicitly overrides the brief:
"**`ENGRAM_STORE` only — never `HOME`.**" That is the right call and it is a lane overriding a
hard constraint on its own authority, which the brief forbids ("violating any of these fails the
lane").

**Pin**: rewrite constraint 1 to name `ENGRAM_STORE`, and give the `tests/cli.rs` helper migration
an owner (CD-6).

### CC-2 — constraint 4's cited precedent does not exist, and `interfaces.md` pins a name that contradicts the constraint

Constraint 4: "write a timestamped backup file next to db.sqlite first (repo precedent:
db.sqlite.pre-v8-backup)".

`grep -rn 'pre-v8|backup' crates/ --include=*.rs -i` finds no backup mechanism anywhere in the
repository. `migrate_v8` (`sqlite.rs:1054`) writes no file. The precedent is not in the tree.

`interfaces.md` §2 then says: "timestamped `db.sqlite.pre-v10-backup` first" — a sentence that
asks for a timestamp and supplies a literal filename without one.

`spec-fact-semantics.md` §4.1 flags the disagreement, follows `interfaces.md`, and asks the lead
to pin one spelling (§6.8). Until that happens the migration's most important safety step has two
specified filenames and no reference implementation.

### CC-3 — constraint 3's "existing JSON fields keep their names and types" versus three deliberate value changes

`spec-fact-semantics.md` changes, on purpose and with argument:

- `suggested_action` loses the value `supersede-old` and gains `requires_context` (§1.4, §4.2);
- `fact_type` for new medications/allergies/diagnoses stops being `profile_slot` (§4.2);
- `arity` for `allergic_to` flips `"single"` → `"multi"` (§7 risk 8);
- `resolved_by_rule` / `facts.resolution` histogram keys change for `birthday`, `died_on`,
  `nationality` (`exact_match` → `slot_lww`, §7 risk 10).

Names and types survive; meanings do not. Constraint 3's other half — "the existing CLI surface
keeps working" — is what an agent scripting `suggested_action == "supersede-old"` will disagree
with. The spec is right that this is the point of the lane; the brief is the document that has to
say so, because contract-codegen will otherwise generate a schema `enum` from the old vocabulary
and a contract test will fail in a lane that did nothing wrong.

### CC-4 — constraint 5's green gate is satisfiable while the guarantee is unproven

Constraint 5 requires `cargo test --workspace` green before commit.
`spec-fact-semantics.md` §5.42 marks the two replay integration tests and the `facts list`
listing assertion `#[ignore]` with the blocking lane named. That is more honest than deleting
them, and it means the lane commits green with move 3's headline property untested and move 2's
cessation behaviour unobserved at the surface.

**Pin**: a lane may commit with `#[ignore]`d tests only if the ignored list appears in its lane
report and in a wave-integration gate that un-ignores them.

### CC-5 — constraint 7 (never touch another lane's files) has no mechanism for the edits the design requires

Constraint 7 forbids editing another lane's files. The design requires, at minimum: one line in
`compile.rs` (CD-1), five changes in `commands/facts.rs` (CW-2), a `pub mod` in
`commands/mod.rs` (no owner, CD-4), a dependency in the workspace `Cargo.toml` (no owner, CD-8),
and a test-helper migration in `tests/cli.rs` (no owner, CD-6).

Every one of these is currently written in a spec as a request to a lane that is scheduled for a
different wave. Without a lead-owned patch channel, the workspace is red between waves and
constraint 5 cannot be met.

### CC-6 — the brief's lane table contradicts itself on interval types

See CM-3: `crates/engram-core/src/**` "(event/interval/handle types)" is storage-journal's, and
fact-semantics' interval algebra is in `engram-graph`.

### CC-7 — the brief describes two defects that are already fixed

Move 4/move 5's `lib.rs:19-21` anchor ("today dispatch creates AppContext before matching") is
stale: `crates/engram-cli/src/lib.rs:19-28` already returns for `Command::AgentInfo` before
`AppContext::new`, with a comment explaining why. `ENGRAM_STORE` likewise already exists in
`paths.rs` with the full config/cache/vectors rooting that `interfaces.md` §5 describes. Two lanes
(daemon-jobs, storage-journal) will otherwise spec work that is done.

---

## 5. Consolidated interface needs — what `interfaces.md` must satisfy

Deduplicated across the brief, `interfaces.md`, `spec-fact-semantics.md` and the landed code.
Exact signatures where a spec gives them; `⚠` marks an item where two sources disagree and the
lead must choose rather than merge.

### 5.1 engram-core — shared types

```rust
// engram_core::handle  (LANDED)
pub enum HandleKind { Memory, Chunk, Fact, Conflict, Entity, Document, Job, Event }
pub struct Handle { pub kind: Option<HandleKind>, pub id: Uuid }
pub enum HandleParseError { UnknownKind(String), BadUuid(String) }
impl Handle { pub fn parse(s: &str) -> Result<Handle, HandleParseError>;
              pub fn render(kind: HandleKind, id: Uuid) -> String; }

// engram_core::events  (LANDED)
pub enum EventKind { MemoryWritten, MemoryRevised, MemoryRetracted, MemoryMetaChanged,
                     DerivationRan, FactObserved, FactIntervalClosed, FactConfirmed,
                     ConflictOpened, DecisionRecorded, DecisionReplayed, CompileRan,
                     JobSubmitted, JobCompleted, JobFailed }
```

**IN-1** ⚠ `Arity` and the relation registry — ONE home. Requested by fact-semantics as
`engram-graph`; required in `engram-core` by the migration (CM-1) and by a structural safety gate
(spec §7 risk 6).

```rust
pub enum Arity { SetValued { safety: bool }, SingleValued { safety: bool } }
pub enum Mutability { Mutable, Immutable }          // fact-semantics addition, §6.3
pub enum Temporality { State, Instant }             // fact-semantics addition, §6.3
pub enum ValueDomain { Date, Text }
pub struct RelationSpec { pub arity: Arity, pub mutability: Mutability,
                          pub temporality: Temporality, pub value: ValueDomain }
impl RelationSpec { pub fn is_safety(&self) -> bool;
                    pub fn is_set_valued(&self) -> bool;
                    pub fn is_single_valued(&self) -> bool; }
pub const REGISTRY_VERSION: &str = "relations@1";
pub const MEMBER_IDENTITY_VERSION: &str = "member_identity@1";
pub fn relation_spec(canonical: &str) -> RelationSpec;
pub fn relation_arity(canonical: &str) -> Arity;
pub fn canonical_relation(raw: &str) -> String;
pub fn cessation_of(raw_or_canonical: &str) -> Option<&'static str>;
pub fn is_safety_slot(canonical: &str) -> bool;
pub fn member_identity(canonical_relation: &str, object_norm: &str) -> String;  // ⚠ CM-11
pub enum LegacyArity { Single, Multi }              // ⚠ CM-4: must NOT be re-exported as `Arity`
pub fn predicate_arity(canonical: &str) -> LegacyArity;
```

Registry v1 contents, pinned: `takes_medication` / `allergic_to` / `diagnosed_with` →
`SetValued{safety:true}`, Mutable, State. `blood_type` → `SingleValued{safety:true}`, Immutable.
`birthday`/`has_birthday`/`date_of_birth`/`born_on`/`died_on` → `SingleValued{false}`, Immutable,
Instant, Date. `nationality` → `SingleValued{false}`, Immutable, State. `lives_in`, `works_at`,
`has_role`, `has_age`, `married_to`, `partner_of`, `prefers_language` → `SingleValued{false}`,
Mutable, State. `married_on`/`founded_on`/`graduated_on`/`joined_on`/`started_on`/`ended_on`/
`hired_on` → `SetValued{false}`, Immutable, Instant, Date. Everything else, including every
current `MULTI_VALUED_PREDICATES` entry → `SetValued{safety:false}`, Mutable, State, Text.

**IN-2** ⚠ Interval algebra — home contested (CM-3); the brief says `engram-core`.

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
pub fn close_at(old: &Interval, successor_from: DateTime<Utc>) -> DateTime<Utc>;  // max(old.from, s)
```

Point-interval algebra is pinned, not left to the implementer: a point interval never overlaps
anything, and every `Instant` relation in registry v1 is also `Immutable` so no `Instant` pair
ever reaches the interval branch.

**IN-3** `Fact` gains, all `#[serde(default)]`, additive:
`closed_by: Option<Uuid>`, `close_reason: Option<String>`, `close_authority: Option<String>`.

**IN-4** `ErrorCode` and its mapping from `StorageError` (CM-10):

```rust
pub enum ErrorCode { BadInput, NotFound, WrongIdKind, Config, Transient, RateLimited,
                     SqliteBusy, ConflictAlreadyResolved, MalformedJson }
```
Exit mapping: BadInput/NotFound/WrongIdKind/MalformedJson/ConflictAlreadyResolved → 3;
Config → 2; Transient/SqliteBusy → 1; RateLimited → 4. **Missing and required**: the
`StorageError → ErrorCode` table, because `StorageError::NotFound` reaches exit 1 today.

**IN-5** `engram-core/src/lib.rs` must re-export `Handle`, `HandleKind`, `EventKind`, `Arity`,
`ErrorCode` — today it re-exports only `CoreError` and seven `types::*` names, so every lane
writes a different `use` path (CM-12).

### 5.2 engram-storage — journal, resolution, facts, decisions

**IN-6** Journal (as pinned in `interfaces.md` §2, unchanged):

```rust
pub struct EventDraft { pub kind: EventKind, pub subject_id: Option<Uuid>, pub kb: Option<String>,
                        pub diary: Option<String>, pub actor: String, pub payload: serde_json::Value }
impl SqliteStore {
    pub fn append_event(&self, draft: &EventDraft) -> Result<(i64, Uuid), StorageError>;
    pub fn events_for_subject(&self, id: Uuid) -> Result<Vec<EventRow>, StorageError>;
}
```
`EventRow` is named in `interfaces.md` and never defined — add it.

**IN-7** Handle resolution:

```rust
pub struct Resolved { pub kind: HandleKind, pub id: Uuid, pub memory_id: Option<Uuid> }
impl SqliteStore { pub fn resolve_handle(&self, h: &Handle) -> Result<Option<Resolved>, StorageError>; }
```

**IN-8** Interval reads and writes (fact-semantics §6.2, verbatim):

```rust
impl SqliteStore {
    pub fn facts_for_relation(&self, kb: &str, diary: &str, subject_norm: &str, relation: &str,
                              include_closed: bool) -> Result<Vec<Fact>, StorageError>;
    pub fn open_intervals(&self, kb: &str, diary: &str, subject_norm: &str,
                          relation: Option<&str>) -> Result<Vec<Fact>, StorageError>;
    pub fn close_fact_interval(&self, fact_id: Uuid, at: DateTime<Utc>, reason: &str,
                               authority: &str, closed_by: Option<Uuid>, actor: &str)
        -> Result<bool, StorageError>;
    pub fn supersede_fact_with(&self, old: Uuid, new: Uuid, resolution: &str,
                               valid_until: Option<DateTime<Utc>>) -> Result<bool, StorageError>;
}
```
⚠ `supersede_fact_with` must not hard-code `authority = "heuristic"` — `facts.rs:630` calls it for
caller decisions (CW-4).

**IN-9** Decision ledger — one owner, one name (CM-6):

```rust
impl SqliteStore {
    pub fn record_decision(&self, kb: &str, diary: &str, key_encoded: &str, verdict: &str,
                           authority: &str, actor: &str, note: Option<&str>,
                           conflict_id: Option<Uuid>, key_version: &str)
        -> Result<Uuid, StorageError>;
    pub fn lookup_decision(&self, kb: &str, key_encoded: &str)
        -> Result<Option<DecisionRow>, StorageError>;   // DecisionRow must be DEFINED here
}
```
Precedence, which the query must implement and not the caller: the most recent `authority='caller'`
row for `(kb, decision_key)`, else the most recent `authority='machine'` row; `verdict='void'`
rows are never selected. Index: `(kb, decision_key, authority, decided_at DESC)`.
A failed lookup propagates — it is never read as "nobody has decided this".

**IN-10** Queue insert, widened, with an unambiguous negative result (CM-7):

```rust
impl SqliteStore {
    pub fn insert_fact_conflict(&self, id: Uuid, new_fact_id: Uuid, old_fact_id: Uuid,
                                diary: &str, kb: &str, detector: &str, score: Option<f32>,
                                suggested: &str, decision_key: &str, member_key: &str,
                                registry_version: &str) -> Result<InsertOutcome, StorageError>;
}
pub enum InsertOutcome { Inserted, DuplicatePair { existing: Uuid }, DuplicateKey { existing: Uuid } }
```

**IN-11** DDL, in this order (fact-semantics §2.6, §4.1 step 5):

```sql
ALTER TABLE facts ADD COLUMN closed_by       TEXT;
ALTER TABLE facts ADD COLUMN close_reason    TEXT;
ALTER TABLE facts ADD COLUMN close_authority TEXT;   -- ⚠ needs an event field, CM-2
CREATE INDEX IF NOT EXISTS idx_facts_open_interval
    ON facts(kb, diary, subject_norm, predicate)
    WHERE valid_until IS NULL AND superseded_by IS NULL;
CREATE INDEX IF NOT EXISTS idx_facts_relation ON facts(kb, diary, subject_norm, predicate);
ALTER TABLE fact_conflict_queue ADD COLUMN decision_key     TEXT;
ALTER TABLE fact_conflict_queue ADD COLUMN member_key       TEXT;
ALTER TABLE fact_conflict_queue ADD COLUMN registry_version TEXT;
CREATE TABLE IF NOT EXISTS fact_decisions (
    id TEXT PRIMARY KEY, kb TEXT NOT NULL DEFAULT 'default',
    diary TEXT NOT NULL DEFAULT 'default', decision_key TEXT NOT NULL,
    subject_norm TEXT NOT NULL, relation TEXT NOT NULL, pair_hash TEXT NOT NULL,
    verdict TEXT NOT NULL, authority TEXT NOT NULL, actor TEXT NOT NULL, note TEXT,
    conflict_id TEXT, event_id TEXT NOT NULL, key_version TEXT NOT NULL, decided_at TEXT NOT NULL);
CREATE INDEX IF NOT EXISTS idx_fact_decisions_key
    ON fact_decisions(kb, decision_key, authority, decided_at DESC);
-- LAST, after the open-row collapse of §4.1 step 5.2:
CREATE UNIQUE INDEX IF NOT EXISTS idx_fcq_open_key
    ON fact_conflict_queue(kb, decision_key) WHERE status='open' AND decision_key IS NOT NULL;
```
`fact_conflict_queue.status` gains `superseded_question` — never `resolved`, because nobody
answered it, and invisible to every `status='open'` listing.

**IN-12** Migration contract, moved out of the fact-semantics spec and into `interfaces.md`
(CW-1). It must state: the backup filename ⚠ (CC-2); that the backup is written outside
`with_tx`, only when the stored version is below 10, and skipped for `:memory:` (CD-7); the
`valid_from`/`valid_until` backfill with the `MAX` clamp; the `resolution → close_reason`
and `resolved_by → authority` maps; genesis event order; `conflict.opened` for **open** queue
rows as well as resolved ones (fact-semantics §6.6.4 — without it the journal cannot rebuild an
open queue and invariant 3 fails on any store with a backlog); the duplicate-key collapse
algorithm and its survivor rule; and idempotency.

**IN-13** The `superseded_by IS NULL` → `is_open_fact()` sweep, as a named contract item with an
owner and the site list of CD-2.

**IN-14** `engram admin rebuild [--kb K]` — required by `interfaces.md` invariant 3, cited as the
architectural rollback, owned by nobody (CD-4).

### 5.3 Event payloads

**IN-15** `fact.interval_closed` gains `authority` and enough to reconstruct the compatibility
write (CM-2):
`(fact_id, valid_until, closed_by?, reason, authority, sets_superseded_by)`.

**IN-16** `decision.recorded` gains `authority: "caller"|"machine"`, an enumerated
`verdict: "keep_both"|"close_old"|"reject_new"|"void"`, and `key_version` (CM-5).

**IN-17** `decision.replayed` is appended by the applier, one per applied replay, in the same
transaction as its effect (invariant 1). Stated in `interfaces.md` §3 as behaviour; make it an
obligation on the write path.

**IN-18** `conflict.opened` payload gains `decision_key`, `member_key`, `registry_version` so a
rebuild restores the queue columns of IN-11.

### 5.4 Classification, keys and replay (fact-semantics owns; other lanes consume)

**IN-19**

```rust
pub enum ConflictClass { Coexist, Duplicate, SupersedeOld, SupersedeNew, Settled, Queue }
pub struct Verdict { pub class: ConflictClass, pub rule: &'static str }
pub enum ClassifyMode { Write, Triage }
pub struct FactView<'a> { pub fact: &'a Fact, pub relation: String, pub spec: RelationSpec,
                          pub member: String, pub interval: Interval }
pub fn classify_pair(new: &Fact, old: &Fact, score: Option<f32>, mode: ClassifyMode) -> Verdict;
pub fn classify_views(new: &FactView<'_>, old: &FactView<'_>, score: Option<f32>,
                      mode: ClassifyMode) -> Verdict;
pub fn safety_gate(v: Verdict, new: &FactView<'_>, old: &FactView<'_>) -> Verdict;
pub fn suggested_action(v: &Verdict, new: &FactView<'_>, old: &FactView<'_>) -> &'static str;
pub fn allowed_actions(v: &Verdict, new: &FactView<'_>, old: &FactView<'_>) -> &'static [&'static str];
```
`safety_gate` covers `SupersedeOld`, `SupersedeNew` **and** `Duplicate`, because `Duplicate` is a
write today (`fact_write.rs:1137-1146`, `facts.rs:635-643`). Its output rule name stays
`"safety_slot"` because `facts.rs:701` reads it as a map key.

**IN-20**

```rust
pub const SEMANTIC_KEY_VERSION: &str = "semantic_key@1";
pub struct SemanticKey { pub subject_norm: String, pub relation: String, pub object_pair_hash: String }
impl SemanticKey { pub fn for_pair(new: &FactView<'_>, old: &FactView<'_>) -> SemanticKey;
                   pub fn encode(&self) -> String;      // subject \u{1f} relation \u{1f} pair_hash
                   pub fn decode(s: &str) -> Option<SemanticKey>; }
pub enum DecisionVerdict { KeepBoth, CloseOld, RejectNew, Void }
pub enum DecisionAuthority { Caller, Machine }
pub struct DecisionRecord { pub key: SemanticKey, pub verdict: DecisionVerdict,
                            pub authority: DecisionAuthority, pub actor: String,
                            pub decided_at: DateTime<Utc>, pub note: Option<String>,
                            pub key_version: String }
pub trait DecisionLedger {
    fn binding_decision(&self, kb: &str, key: &SemanticKey)
        -> Result<Option<DecisionRecord>, StorageError>;
}
pub fn replay(prior: &DecisionRecord, v: Verdict) -> Option<Verdict>;
```
⚠ **IN-20a** `object_pair_hash = hex(blake3(min(a,b) || 0x1f || max(a,b)))` requires blake3 in
`[workspace.dependencies]`, or the lead picks `Uuid::new_v5`. One choice, before the migration is
written (CD-8).
The key is scoped by `kb` at lookup and deliberately not by `diary`.

**IN-21** The pure planner, so the polypharmacy guarantee is provable without a store:

```rust
pub struct Observation { /* fact_id, subject, subject_norm, relation, spec, object, object_norm,
                            member, confidence, source_memory_id, source_kind, interval,
                            state: ObservationState, autonomy: Autonomy, queue_everything: bool */ }
pub enum ObservationState { Ongoing, Ended }
pub struct PlannedFact { pub id: Uuid, pub fact_type: FactType, pub interval: Interval,
                         pub close: Option<(CloseReason, CloseAuthority)> }
pub struct PlannedClose { pub fact_id: Uuid, pub at: DateTime<Utc>, pub reason: CloseReason,
                          pub authority: CloseAuthority, pub closed_by: Option<Uuid>,
                          pub resolution_tag: &'static str }
pub struct PlannedConflict { pub new_fact_id: Uuid, pub old_fact_id: Uuid,
                             pub detector: &'static str, pub score: Option<f32>,
                             pub verdict: Verdict, pub key: SemanticKey,
                             pub suggested: &'static str, pub allowed: &'static [&'static str],
                             pub member_key: String, pub registry_version: &'static str }
pub enum PlanNote { UnreadableValidUntil { raw: String }, ValidUntilBeforeValidFrom { raw: String },
                    EndWithoutOpenInterval { member: String },
                    ReplayApplied { key: String, verdict: DecisionVerdict } }
pub struct WritePlan { pub insert: Option<PlannedFact>, pub confirm: Vec<Uuid>,
                       pub closes: Vec<PlannedClose>, pub queue: Vec<PlannedConflict>,
                       pub replays: Vec<PlannedReplay>, pub notes: Vec<PlanNote> }
pub fn plan_write(observation: &Observation, siblings: &[Fact], others: &[(Fact, f32)],
                  mode: ClassifyMode,
                  prior: &dyn Fn(&SemanticKey) -> Result<Option<DecisionRecord>, StorageError>)
    -> Result<WritePlan, StorageError>;
```
`PlannedReplay` is used and never defined — add it. `Autonomy::Strict` and
`ENGRAM_FACT_QUEUE_ALL` live in the planner, not the applier, so a plan can never assert a close
the applier refuses.

### 5.5 Extraction

**IN-22** `ExtractedFact` gains `#[serde(default)] valid_until: Option<String>` and
`#[serde(default)] state: Option<String>` ("ongoing" | "ended") plus a `Default` impl.
Consequence: `compile.rs:866-875` needs `..Default::default()` — one line, another lane, another
wave (CD-1).

### 5.6 Envelope, errors and the response builder

**IN-23** (from `interfaces.md` §4, unchanged and still required) `returned` / `total` /
`cursor` on every listing with legacy `count` = returned; `metadata.scope { kbs, diary,
chunks_considered }`; `data.verdict: "results" | "no_match"` with `data.absence { top_score,
threshold, searched_kbs, searched_chunks }`; `facts_invalidated` and
`reextraction: "replayed"|"pending"|"none"` on edit/update; `elapsed_ms` measured once in dispatch.

**IN-24** `resolve { program: "engram", args: [...], allowed_actions: [...] }` with the legacy
`resolve_command` string carrying ONE runnable command and no pipe. ⚠ Needs a single builder
owned by contract-codegen in wave 2, not by daemon-jobs in wave 4 (CM-9), because three wave-3
files emit it.

**IN-25** ⚠ `suggested_action`'s enumerated value set — safety-only narrowing (brief-exact) or
the blanket rule (fact-semantics §1.4). Contract-codegen cannot generate a schema until this is
decided (CM-8).

**IN-26** Source kinds: `--facts-json` should stamp `SourceKind::UserStated` rather than
`AgentExtracted` (`remember.rs:632`), or the `correction_by_user` rule is unreachable in
production for every relation (fact-semantics §6.7). Write-surfaces' call.

### 5.7 Store resolution, tests and ownership

**IN-27** ⚠ `ENGRAM_STORE`, not `ENGRAM_HOME` (CM-13); precedence `--store <dir>` >
`ENGRAM_STORE` > ProjectDirs, with the flag's three-lane split resolved (CD-5).

**IN-28** ⚠ Test isolation: one mechanism, stated in both documents (CC-1), plus an owner for the
`tests/cli.rs` helper migration and for the `wave0_*` / `wave05_*` files that no lane owns (CD-6).

**IN-29** Ownership additions the table is missing: `crates/engram-cli/src/commands/mod.rs`, the
workspace `Cargo.toml`, `crates/engram-graph/Cargo.toml`, `crates/engram-cli/src/commands/skill.rs`
(which `include_str!`s the asset contract-codegen owns), `crates/engram-cli/tests/cli.rs`,
`wave0_*.rs`, `wave05_*.rs`, and the unowned commands `doctor.rs`, `repair.rs`, `chunks.rs`,
`ingest.rs`, `kb.rs`, `reindex.rs`, `update.rs`, `usage.rs`, `config.rs`, `budget.rs`, `bench.rs`.
The brief also writes the skill asset as `assets/skill.md`; the path is
`crates/engram-cli/assets/skill.md`.

**IN-30** A cross-lane patch channel: a named mechanism by which a one-line change in another
lane's file lands with the change that requires it, so the workspace is never red between waves
(CC-5).

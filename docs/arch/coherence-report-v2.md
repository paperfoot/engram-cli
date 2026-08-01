# coherence report v2 — arch/v0.4 cross-lane judging, all seven specs

Judged 2026-08-01 against `docs/arch/redesign-brief.md`, `docs/arch/interfaces.md` **v2**
(rulings R1–R15), the previous `docs/arch/coherence-report.md` (67 findings), the seven
`docs/arch/spec-*.md`, and the tree at `a239e0e` on `arch/v0.4`.

## 0. What changed since v1, and the one thing that did not

Six of the seven specs were written against interfaces v2 and cite its rulings. One was not:

| spec | R-ruling citations | reads interfaces v2 | verdict |
|---|---|---|---|
| spec-storage-journal.md | 26 | yes (23 refs) | v2-conformant |
| spec-contract-codegen.md | 33 | yes | v2-conformant |
| spec-daemon-jobs.md | 20 | yes | v2-conformant |
| spec-read-surfaces.md | 17 | yes | v2-conformant |
| spec-write-surfaces.md | 13 | yes | v2-conformant |
| spec-reasoning-layer.md | 11 | yes | v2-conformant |
| **spec-fact-semantics.md** | **0** | **0** | **pre-v2 throughout** |

`rg -c 'R1[0-5]|R[1-9]\]' docs/arch/spec-fact-semantics.md` returns nothing. The lane whose
rulings R1, R3, R5a, R6, R7, R9 and R10 were *written about* is the one lane that never read
them. Every violation in §1 except V8 and V9 is a consequence of that single fact, and the
cheapest fix in this whole report is one revision pass over that document before wave 3 starts.

Findings carry stable ids: **V** ruling violations, **M** new mismatches, **S** unstated
dependencies/sequencing, **W** duplicated work, **C** hard-constraint contradictions,
**D** consolidated interface delta. Within each section, most severe first.

---

## 1. Violations of interfaces v2 rulings

### V1 — spec-fact-semantics still implements the blanket `supersede-old` removal that R7 overruled

R7: "Vocabulary stays `keep-both | supersede-old | requires_context`. … The blanket removal in
spec-fact-semantics §1.4 is overruled."

The spec still carries the overruled rule in seven places (§1.1 item 4; §2.3's
`suggested_action` doc comment "Never `supersede-old`"; §2.3.1's two-arm suggestion table;
§2.4's `queue_conflict` bullet; §4.2's compatibility note "losing one it emitted
(`supersede-old`)"; §5 test 17 `suggested_action_is_never_supersede_old`, a *property test* that
pins the overruled behaviour; §7 risk 9, which still presents it as an open question and says
"The spec currently implements the blanket rule"). Full change list in §6.

Compounding: R7's honest case — `supersede-old` for non-safety single-valued pairs that queued
only because autonomy was `strict` — is **not computable from the spec's own data model**. §2.3.3
step 9.5 escalates a `SupersedeOld` close into a `queue` entry, and `PlannedConflict.verdict`
then carries `Queue`; the pre-escalation class is discarded, so `suggested_action` cannot tell
"queued because strict" from "queued because comparable". Implementing R7 requires a struct
change (D18), not a table edit.

### V2 — spec-fact-semantics specifies blake3, which R9 rejected

§2.3.2 line 465: "`object_pair_hash = hex(blake3(min(a,b) || 0x1f || max(a,b)))`". §3's file
table: "`crates/engram-graph/Cargo.toml` | EDIT. add `blake3.workspace = true`". §6.1: "This
lane will use blake3 as specified unless the lead says otherwise."

R9: "No new dependency. `object_pair_hash = Uuid::new_v5(SEMANTIC_KEY_NAMESPACE, …)` hex-encoded
… blake3 is rejected." Storage-journal already implements the uuid5 form. Shipping fact-semantics
as written produces two key encodings, and R9's own reason applies: the choice is baked into
~14.5k migrated decision rows, so a mismatch silently stops every migrated decision from
replaying — the exact failure move 3 exists to prevent.

### V3 — spec-fact-semantics builds `relations.rs` and `interval.rs` in `engram-graph`, which R1 moved to `engram-core` and which have already landed there

§2.1 "New: `crates/engram-graph/src/relations.rs`", §2.2 "New:
`crates/engram-graph/src/interval.rs`", and §3's file table budgets ~420 and ~280 new lines for
them. R1/interfaces §0 put both in `engram_core::{relations, interval}`, storage-journal landed
them at `dbdb757`, and storage-journal §2.1 says explicitly "LANDED. This lane verifies, it does
not rewrite."

Implemented as written, fact-semantics *creates* CM-1 and CM-3 — two registries, two member
heuristics, two interval algebras, one of which the migration cannot reach — in the wave that was
supposed to close them.

Same family: §2.1 writes `pub use engram_core::types::Arity;` (CM-12's wrong module; the type is
in `engram_core::relations`), and §6.3 asks that `Mutability`/`Temporality` "live in
`engram-graph::relations`" when they landed in `engram_core::relations`.

### V4 — spec-fact-semantics re-exports `LegacyArity as Arity`, which R3 forbids by name

§2.1: "`fact_write.rs` re-exports it under BOTH spellings: `pub use
engram_graph::relations::LegacyArity as Arity;` plus `pub use …::LegacyArity;`".

R3: "The legacy projection keeps a legacy NAME at every call site. `facts.rs` et al are renamed
to `LegacyArity`; **nothing re-exports it as `Arity`**."

Read-surfaces §3.4.1 implements R3 correctly (imports `LegacyArity` from `engram_core::relations`
and drops the `fact_write::Arity` import at `facts.rs:20-24`, verified present in the tree). So
the alias would survive as a public trap in `fact_write.rs` with no in-repo consumer — a
`pub use` whose only function is to make the next reader of `Arity` wrong.

### V5 — spec-fact-semantics keeps `insert_fact_conflict -> Result<bool>`, which R10 replaced

§6.2 pins `insert_fact_conflict(…) -> Result<bool, StorageError>`, and §2.4 builds a recovery on
`Ok(false)` ("look up the open row for `(kb, decision_key)` and push a `queued` entry naming THAT
`conflict_id`, with `deduplicated: true`"). That recovery is *precisely* what CM-7 showed to be
wrong for the `DuplicatePair` case — it can return a conflict id for a different pair, or nothing
at all when the existing row predates the backfill.

R10 replaced it with `InsertOutcome { Inserted, DuplicatePair{existing}, DuplicateKey{existing} }`.
Storage-journal §2.6 implements R10 and CLP-6 hands fact-semantics the call-site change.
Fact-semantics never adopted it, so the applier is written against a return type storage will not
have.

### V6 — spec-fact-semantics pins the un-timestamped backup filename that R5a overruled, and interfaces §2.1 still names that section as its binding annex

§4.1 Phase 1: "The backup filename is `db.sqlite.pre-v10-backup`, exactly as interfaces.md §2
spells it … until then the implementer follows interfaces.md." §6.8 asks the lead to pin one
spelling.

R5a already did: `db.sqlite.pre-v10-backup.<YYYYMMDDHHMMSS>`, via `VACUUM INTO`, and it says the
v1 filename "and constraint 4's 'repo precedent' were both wrong". Storage-journal §4.0 follows
R5a. The hazard is not the filename — storage owns the code — it is that **interfaces §2.1
declares spec-fact-semantics §4.1 its binding annex**, so the contract currently points at a
document that contradicts the ruling in the same clause.

### V7 — spec-fact-semantics edits lead-owned test files

§5.41 requires rewriting `cli.rs:1791` and `wave05_compiled_facts.rs:170` and says "Both
rewrites go in their own commit", i.e. the lane's. R15 gave `crates/engram-cli/tests/cli.rs`,
`tests/wave0_*.rs` and `tests/wave05_*.rs` to the lead. These two rewrites are correct and
necessary (the assertions *are* the polypharmacy bug) — they are patch-channel items, not lane
commits.

### V8 — storage-journal relocates R9's `SEMANTIC_KEY_NAMESPACE` out of `engram_graph` (declared, blocking)

R9 pins `pub const SEMANTIC_KEY_NAMESPACE: Uuid` in `engram_graph`; interfaces §0 puts
`SemanticKey` there. Storage-journal §2.1 puts the namespace, `SEMANTIC_KEY_VERSION`,
`object_pair_hash`, `semantic_key_encode/decode` and `content_hash` in a new
`engram_core::keys`, because `engram-storage` cannot depend on `engram-graph` and the v9→v10
migration must key 14.5k decision rows.

This is declared honestly as OQ-1 and the spec's own status line says the migration is **blocked**
on it. It is listed here because it is a live ruling conflict that blocks wave 2, not because the
lane hid it. The proposal is correct and R9 should be amended (D15): primitives in
`engram_core::keys`, `SemanticKey` + `for_pair` stay in `engram_graph` and delegate. R9 also never
said whether the pair components are the two `object_norm`s or the two member identities; the
migration and the classifier must agree, and only `object_norm` is available to the migration.

### V9 — interfaces §2.1 step 2 names a column that does not exist

"`resolution` → `close_reason` and `resolved_by` → `authority`". `facts` has no `resolved_by`
column (`types.rs:184` and the DDL carry `resolution` only; `resolved_by` is on
`fact_conflict_queue`). Storage-journal flags this as OQ-17 and maps both outputs from
`resolution`, which is what the annex does. The contract text needs correcting, not the code.

### V10 — two lanes own `crates/engram-core/src/error.rs`

The brief gives `crates/engram-core/src/**` to storage-journal; storage-journal landed `ErrorCode`
there at `a239e0e`. spec-contract-codegen's owned-files table claims the same path, citing R11's
"`error.rs` (contract-codegen) implements". R11 plainly meant `crates/engram-cli/src/error.rs`
(the mapping table), but the sentence is ambiguous and contract-codegen §8.2.1 asks for a ruling.
Two exclusive owners of one file is what constraint 7 forbids and what R15's patch channel exists
to route around; the patch list (2a/2b) is already written, so the ruling is one line.

### V11 — minor, but they will each cost a review cycle

- fact-semantics §5.2's integration harness sets `HOME`, three `XDG_*` vars **and**
  `ENGRAM_STORE`. R14: "new tests use `ENGRAM_STORE` alone."
- contract-codegen §6.6's `skill_install_then_doctor_reports_fresh` sets `HOME` deliberately
  (`skill.rs` writes under the home directory). This is the one defensible exception in the build
  and needs blessing in interfaces, not a footnote in a lane spec.
- fact-semantics §6.7 still frames `SourceKind::UserStated` as a request write-surfaces "may
  decline"; R13 made it binding and write-surfaces W6 implements it.

---

## 2. New interface mismatches between lanes

### M1 — `facts resolve` has two incompatible implementations, in two waves, and the wave-4 one double-writes the journal

read-surfaces §3.4.8 (wave 3) rewrites `facts.rs:451-517` around one storage call:

```rust
match ctx.store.resolve_conflict_with_decision(qid, kb, diary, &key.encode(),
        verdict, "caller", actor, note, SEMANTIC_KEY_VERSION)? { … }
```

daemon-jobs §10.3 (wave 4) rewrites the *same function* around a different one — "`pub fn
resolve(...)` becomes `pub async fn resolve(...)` … `ctx.core.resolve_conflict(req).await?` …
All five statements of the current non-transactional sequence are deleted" — and §5.2 spells out
a body that:

1. claims with **`store.resolve_fact_conflict(id, "caller", note)`**, the method storage-journal
   §2.11 explicitly keeps *unchanged and decision-free* ("records no decision; new callers use
   `resolve_conflict_with_decision`");
2. appends `fact.interval_closed` itself (step 6) — but it reached that point through
   `supersede_fact_with`, which storage §2.8 re-expresses over `close_fact_interval`, **which
   already appends `fact.interval_closed`**. Two events per close;
3. appends `decision.recorded` itself (step 7) and then calls `record_decision(...)` passing the
   `event_id` — but storage §2.5's `record_decision` "appends `decision.recorded` with the full R4
   payload … inserts the projection row with `event_id` = the appended event". Two events per
   decision.

Consequences, in order of how quietly they fail: `admin rebuild` replays each close and each
decision twice (invariant 3's byte-identical property fails on any store where a conflict was
resolved); the two events for one close carry **different payloads** (see M4); and read-surfaces'
wave-3 work is deleted in wave 4 by a lane that does not own the file. Neither spec cites the
other's plan for this function.

**Resolution required before wave 3**: one implementation. `resolve_conflict_with_decision` is the
storage primitive; `Core::resolve_conflict` in wave 4 is a thin async wrapper that adds the cycle
guard (§5.3) and the `may_close` gate (§5.4) and appends nothing.

### M2 — `edit`'s conditional re-extraction marker clear is impossible as specified, and the clear is unjournalled, so `admin rebuild` silently undoes every pending re-extraction

Two independent defects on one line.

**(a) The condition cannot be expressed.** write-surfaces E7a is careful: inside the transaction,
consult the derivation ledger first, then `close_facts_for_memory` runs either way but
`clear_extraction_marker` runs **only when `!replayed`** — that ordering is the whole of W4's
`reextraction: "replayed" | "pending" | "none"` contract, and the spec says so ("the earlier draft
made this decision AFTER the transaction while E3 cleared the marker unconditionally INSIDE it, so
`"replayed"` could never be honest"). But storage-journal §3.2 rewrites the same method as:

> `:1925-1935` `delete_facts_for_memory` … closes each of the memory's open fact intervals with
> `retracted_source` **and deletes the `fact_extraction_log` marker only**. Renamed
> `close_facts_for_memory`…

So storage's `close_facts_for_memory` always clears the marker. The two halves write-surfaces
split apart (WR-1 and WR-2) are welded back together in the method it calls. Signature conflict
too: storage's is the renamed `(memory_id)`; write-surfaces WR-1 needs
`(memory_id, at, reason, authority, actor) -> Vec<Uuid>`.

**(b) Clearing the marker is not an event, so a rebuild resurrects it.** storage §2.4 has
`record_derivation` write both the `derivations` row and the legacy `fact_extraction_log` marker;
§2.12's rebuild dispatcher restores `fact_extraction_log` from `derivation.ran`. Nothing appends
an event when a marker is *cleared*. After `edit --content` (marker cleared, memory pending) an
`admin rebuild` replays the original `derivation.ran` and re-inserts the marker — the memory stops
being pending, `facts backfill` never revisits it, and the facts that `edit` closed as `corrected`
are never re-derived. That is the brief's re-extraction time bomb, reintroduced by the rebuild
that was supposed to make everything reconstructible.

Neither lane names either half. Fix in D5/D6.

### M3 — `respond.rs` has three incompatible APIs across four lanes; R12 pinned the module, not its signatures

| lane | what it calls |
|---|---|
| contract-codegen §2.4 (owner) | `resolve_conflict(conflict_id, allowed_actions) -> ResolvePayload`, `attach(&mut Map, &p)`, `resolve_json`, `resolve_command_string`, `retry_with_handle` |
| read-surfaces §3.4.8, §6.4 CC-R1 | the same names (it withdrew its divergent one — the only lane that reconciled) |
| reasoning-layer §3.1, §6.2 C-6 | `respond::resolve_payload` — a name that exists in no spec |
| daemon-jobs §3.8, §10.2 | `respond::{success, error, listing, conflicts, resolve_block}` — five names contract-codegen does not ship, and `resolve_block(args, allowed_actions)` takes different arguments than `resolve_conflict(conflict_id, allowed_actions)` |

daemon-jobs escalates this itself (OQ-8) and states the one outcome CM-9 forbids: "what this lane
will **not** do is ship a fourth private envelope layer". It is also blocking — daemon's D6
(`/v1/recall` conflicts) cannot start until the envelope half has a home. `ResolveAction` is
additionally defined in *both* contract-codegen §2.4 and daemon-jobs §5.2 and must collapse to
one definition.

R12 gave the module an owner and a wave and one sentence of content. It needs the full surface
(D1), including a ruling on whether the envelope builders live there or in `output.rs`.

### M4 — `output::{Listing, Scope, Absence}` ship in wave 2 with field types their wave-3 consumer cannot use

contract-codegen §2.7 ships the types; read-surfaces §2 explicitly refuses to declare a parallel
set (correctly) and then needs three changes to them:

| field | codegen §2.7 | read-surfaces needs | reasoning-layer needs |
|---|---|---|---|
| `Listing.total` | `usize` | `i64` (a SQL `COUNT(*)`) — CC-R4 | — |
| `Scope.chunks_considered` | `usize`, always present | `Option<usize>`, omitted on non-retrieval surfaces — CC-R5 | omitted / `0` |
| `Scope.origin` | absent | `&'static str`, vocabulary `flag\|config\|all_kbs` — CC-R5 | — |
| `Absence.threshold` | `f32` | `Option<f32>`; "**this lane does not ship `absence.threshold` at all rather than ship a zero**" — CC-R6 | `Some(0.92)` (a Jaro-Winkler threshold, not a score cutoff) |
| `Absence.reason` | absent | `&'static str` with a five-value vocabulary — CC-R6 | — |
| `Absence.nearest_subjects` | absent | — | `[{subject, score}]` — X-1 |

read-surfaces states it plainly (OQ-10): "Whichever way it is ruled, one of the two specs is
edited before wave 2 ships `output.rs`." This is the tightest deadline in the build: the ruling is
needed **before wave 2 writes the file**, not at the wave-3 gate, because `contract_envelope`
fails on whichever spelling loses and read-surfaces will otherwise hand-roll its own.

Two further wrinkles nobody costed: `Absence.threshold` means two different things on the two
surfaces that emit it (a retrieval cutoff that does not exist, and a subject-resolution
threshold that does), and read-surfaces additionally invents `metadata.scope.diary_applicable`,
`metadata.command_alias` and `metadata.suggested_action_overridden`, none of which is in
interfaces §4 or in codegen's registry.

### M5 — one user action, two `close_reason` values, two rendered statuses

`engram facts resolve <id> --action supersede-old`:

- **storage-journal §2.8**: `supersede_fact_with(old, new, resolution = "caller", …)` →
  `close_reason` is "always `superseded` … except `resolution = "caller"` → `corrected`", and
  `close_authority = caller`.
- **daemon-jobs §5.2 step 6**: appends `fact.interval_closed` with
  `payload: { …, reason: "superseded", authority: "caller", sets_superseded_by: true }`.
- **read-surfaces §3.3.6** renders the conflict-entry `status` from `close_reason`:
  `superseded → "superseded"`, `corrected → "corrected"`.

So the same caller decision produces `status: "corrected"` or `status: "superseded"` depending on
whether it went through the CLI's storage call or the daemon's `Core`, and (per M1) both events
exist for the same close. read-surfaces §4.1 books the widened `status` value set as "the one
value-semantics change in the lane" and documents it in agent-info; it cannot document two
answers.

Adjacent and already half-flagged: storage OQ-13 and write-surfaces Q10 disagree about
`edit`/`forget` closures (`retracted_source`+`explicit_end` vs `corrected`/`caller`), and
write-surfaces invented a third wire form, `"caller:<actor>"`, which `CloseAuthority::as_str()`
(landed, `interval.rs:133-141`) does not produce. R2 makes `close_authority` rebuildable from the
event, so exactly one string per authority is mandatory. Ruling in D5/D19.

### M6 — `surface_conflicts` gets two signatures, and the daemon's drops the `diary` parameter the whole deliverable exists to add

read-surfaces deliverable 5 exists because "`recall.rs:305,336` conflicts ignore `diary` —
cross-diary leak into the conflicts array", and §3.3.6 changes the function to
`fn surface_conflicts(ctx, kb: Option<&str>, diary: Option<&str>, query: &str, limit: usize) ->
Result<ConflictSurface, CliError>`.

daemon-jobs §10.3 asks read-surfaces for
`pub fn surface_conflicts_for(store: &SqliteStore, kb: Option<&str>, query: &str)` — no `diary`,
no `limit`, a different return type, a different name. Wired as written, `/v1/recall` becomes the
one surface that still leaks another diary's contradictions, in the wave after the CLI stopped.
(daemon's review already caught the *other* half — that lifting the function as-is carries the
piped `resolve_command` into HTTP — and patched that; the dropped `diary` survived.)

### M7 — three specs emit `provenance.kb` for a fact; `Fact` has no `kb` field

`engram_core::types::Fact` (verified, `types.rs:151-191`) carries `diary` and no `kb`; the `facts`
table has a `kb` column and `insert_fact` derives it with a subquery, which storage-journal §2.12
calls out as a rebuild hazard. Meanwhile:

- read-surfaces §2.4 declares `Provenance { …, pub kb: String, … }` — non-optional — and §2.6
  emits `"provenance": { …, "kb": …, "diary": … }` on every `fact_to_json` row;
- reasoning-layer §2.1 reports `metadata.scope.kbs` per state group and needs to know which KB a
  fact came from to render it;
- storage-journal D10 adds three `Fact` fields (`closed_by`, `close_reason`, `close_authority`)
  and not `kb`.

There is no source for the value. Either `Fact` gains `kb` (D3 — additive, `#[serde(default)]`,
and it also removes the rebuild hazard) or `Provenance.kb` becomes `Option<String>` and the
listings stop claiming it.

### M8 — `AdminCommand` has two pre-agreed clap definitions, and the destructive-command guard is in only one

- storage-journal §2.12 makes `--confirm` mandatory for a non-`--dry-run` rebuild ("its absence is
  `BadInput` (exit 3) with the exact command to re-run"), and CLP-2 supplies
  `Rebuild { kb, dry_run, confirm }`; `commands/admin.rs::rebuild(ctx, kb, dry_run, confirm)`.
- contract-codegen §4.7 pre-agreed `AdminCommand { Rebuild { kb, dry_run } }` — no `confirm`.

Loud rather than silent (the dispatch arm will not compile), but it is a destructive command whose
only guard exists in one of the two documents that define its surface. Same class, quieter:
reasoning-layer C-1 explicitly supersedes contract-codegen §4.7's `State` variant, which lacks the
`--limit`/`--cursor` that interfaces §4 requires of every listing — reasoning-layer flags this as
its risk 1b and says the choice "is not this lane's to make".

### M9 — `EventKind` is a pinned closed set that reasoning-layer must widen, and the rebuild dispatcher errors on any kind it does not know

`engram_core::events::EventKind` has 15 variants and `ALL`; storage-journal §2.9b states the rule
("No new `EventKind` is invented for this — the 15 kinds are pinned") and §2.12 makes the rebuild
dispatcher fail hard on an unknown kind (`StorageError::MalformedRow` — "a skipped event is a
projection that silently diverges from its journal").

reasoning-layer S-2 needs three more (`lesson.recorded|confirmed|retracted`, `ALL: [_; 18]`), S-4
needs the rebuild arms, and X-3 needs the payloads in interfaces §2. If S-2 lands without S-4,
**`engram admin rebuild` errors on any store where someone ran `state learn`** — a wave-5 feature
bricking a wave-2 command. If neither lands, `state lessons/learn/reflect` degrade to the
memory-only fallback in reasoning-layer §4.4.

Same shape for `HandleKind` (also closed, also landed): read-surfaces CC-R2b wants a `wiki`
variant or a ruling that wiki pages carry no handle; reasoning-layer X-2 wants `lesson`. Both
lanes correctly refuse to print a prefix `Handle::parse` would reject. Both are one ruling.

### M10 — `record_decision` has two signatures and interfaces has a third that cannot be right

- interfaces §3.4: `record_decision(&self, /* fields as DecisionRow minus id/event_id */)`.
- storage-journal §2.5: `(kb, diary, key_encoded, verdict, authority, actor, note, conflict_id,
  key_version) -> Result<Uuid>`, decomposing the key internally and appending the event itself.
- daemon-jobs §5.2 step 7: a 13-argument call passing `subject_norm`, `relation`, `pair_hash`
  **and `event_id`**, because it appended the event first.

daemon flags the arity question (OQ-9) but not the conflict with storage's plan. The interfaces
wording is the root: `DecisionRow` is frozen *including* `event_id`, and "minus id/event_id"
implies the callee mints both — which is storage's reading and the right one. Pin it (D7).

### M11 — read-surfaces and reasoning-layer each build a cursor codec; daemon builds a third

- read-surfaces §2.1: `read_surface::{CursorKey, encode_cursor, decode_cursor, next_cursor}` —
  `"1." + base64url({s,f,o})`, fingerprint-bound to the request scope, `BadInput` on mismatch.
- reasoning-layer §3.1: private `encode_cursor/decode_cursor` in `state.rs`, "opaque base64 of
  `(relation, fact_id)`", no fingerprint.
- daemon-jobs §4.7: `GET /v1/jobs?cursor=` with no codec named at all.

`cursor` is one documented wire field with one documented rule ("opaque; pass it back verbatim").
Three encodings means a cursor from one surface silently decodes as garbage on another, and the
fingerprint check — the thing that stops a cursor paging a different result set — exists on one of
the three. `read_surface.rs` should be the one home; reasoning-layer should call it (which costs
reasoning-layer a `use` and nothing else, since `read_surface.rs` is pure functions).

### M12 — `Provenance` is defined twice, differently, under one JSON key

- read-surfaces §2.4: `{ chunk_id, memory_id, document_id, kb, diary, derived_from }`, with a
  per-hit-kind resolution table and nulls preserved.
- reasoning-layer §2.2: `{ memory_id, chunk_id, document_id, observed_at, snippet }` — no `kb`,
  no `diary`, no `derived_from`, plus two fields read-surfaces does not have.

Both serialize to `"provenance"`. Move 5's promise is "every read emits full provenance chain";
two shapes means an agent cannot write one parser. One struct, in `read_surface.rs`, with
`observed_at`/`snippet` as optional additions.

### M13 — `Interval::is_open()` and `is_open_fact()` disagree on one row shape

Landed code: `Interval::of` sets `until = valid_until.or(superseded_at if superseded_by.is_some())`,
`Interval::is_open()` is `until.is_none()`, and `is_open_fact(f)` is
`valid_until.is_none() && superseded_by.is_none()`. A row with `superseded_by` set and
`superseded_at` NULL — which `import_fact_row` can restore from a v2 export, and which
storage-journal §4.2's third backfill case exists precisely to handle in the mirror direction —
yields `Interval::is_open() == true` and `is_open_fact() == false`.

Every lane uses one or the other as "is this current": fact-semantics' classifier reads intervals,
read-surfaces' sweep reads `is_open_fact`, reasoning-layer reads `open_intervals`. One predicate.
Either `Interval::of` falls back to `created_at` when `superseded_by` is set, or `is_open()`
consults the fact. Cheap to fix in wave 2, expensive to find in wave 4.

### M14 — reasoning-layer's `--as-of` helper deletes every `Instant` fact, and its own test says so

§3.2 step 4a pins:

```rust
fn contains(iv: &Interval, t: DateTime<Utc>) -> bool {
    if iv.is_point() { return iv.from == t; }
    iv.from <= t && iv.until.map_or(true, |u| u > t)
}
```

Test 13b requires "a `birthday` … is present under `--as-of` at its own instant **and under
`--as-of` at any later date**". The pinned function returns `false` for every `t` except the exact
instant, so `state ada --as-of 2026-01-01` drops the birthday — the failure mode the paragraph
above it says it exists to prevent. An `Instant`+`Immutable` fact is true from its instant onward
for state purposes; the point branch must be `iv.from <= t`.

### M15 — the extractor id string has no owner and three spellings

storage-journal §2.4 pins `EXTRACTOR_FACT_LLM = "fact_llm@1"` and `EXTRACTOR_COMPILE =
"compile_llm@1"` in `journal.rs`. write-surfaces E7b invents `EDIT_EXTRACTOR_ID` and says "This
lane does not invent it; if fact-semantics/daemon-jobs have not pinned it by integration,
`replayed` stays `false`" — while the constant it needs already exists in a third lane's file.
daemon-jobs §4.5 gates `jobs resume` on `derivation_cache_available()` keyed by the same string.
A mismatch here is not a compile error; it is a permanent 0% cache hit rate and a
`reextraction: "replayed"` that never fires.

### M16 — `delete_document` is the fourth hard-delete path and storage's enumeration misses it

storage-journal §2.9b enumerates "three more live paths in this lane's own file [that] delete fact
rows": `delete_kb`, `repair_integrity`, `hard_delete_memory`. Verified in the tree,
`sqlite.rs:2535-2553` is a fourth: `delete_document` loops `delete_facts_for_memory` over the
document's memories, then soft-deletes those memories with a bare `UPDATE memories SET deleted =
1` — **bypassing `soft_delete_memory` entirely, so no `memory.retracted` event is appended**.
Reachable from `documents delete`, `serve.rs:263` and re-ingest (`ingest.rs:289`).

After storage's rename, the fact half self-heals (the alias closes rather than deletes). The
memory half does not: a document delete retracts N memories with no journal record, which
invariant 1 forbids and which export v3 and any audit will simply not show. write-surfaces risk 12
flagged the path but concluded "no lane owns it" — `sqlite.rs` is storage-journal's, so it is
owned; it is only unenumerated.

### M17 — `agent-info`'s safety sentence is corrected in one lane and left in place in the tree it describes

contract-codegen §3.2 rewrites `agent_info.rs:41`'s "Safety slots (allergic_to, takes_medication,
diagnosed_with) are never auto-superseded" to the R7/move-2 wording, correctly (CW-6). But it also
pins the rule "**the contract describes the binary that exists, never the binary that is
coming**", and this sentence describes behaviour that only becomes true when fact-semantics lands
in wave 3. Under codegen's own discipline the corrected sentence is a `since_wave: 3` entry;
under the current text it ships in wave 2 describing a binary in which `fact_write.rs:1209-1213`
still emits `suggested_action: "supersede-old"` on safety pairs. codegen's risk 7 describes
exactly this window for the *value*; it does not apply the same rule to the *prose*.

---

## 3. Unstated cross-lane dependencies, and what they do to the sequencing

### S1 — OQ-1 blocks wave 2, which blocks waves 3–5

storage-journal's status line: "ready to implement **except the migration's key encoding, which is
blocked on OQ-1** … the ~14.5k decision rows must not be written until the lead rules where
`SEMANTIC_KEY_NAMESPACE` lives." Everything downstream of the migration — decision replay
(fact-semantics), `facts resolve` recording a decision (read-surfaces), replay across an edit
(write-surfaces), `Core::resolve_conflict` (daemon) — inherits the block. This is the single
highest-leverage ruling in the report and it is one paragraph (D15).

### S2 — read-surfaces needs nine storage methods that storage-journal's wave-2 deliverable list does not contain

read-surfaces §6.2 states its own acceptance gate: "**none of SR-1…SR-3, SR-6, SR-7, SR-9 appear
in `spec-storage-journal.md` §1.1's deliverable list or in interfaces.md today** … Deliverables
1, 2 and 5 do not compile until storage-journal accepts them."

Concretely: `memory_ids_for_chunks`; `diary` on `facts_matching_terms` /
`open_conflicts_matching_terms` (+ a count); `list_recent_conflicts(diary, kb, limit, offset)` +
`count_recent_conflicts`; `idx_facts_scope_open`; `count_facts_scoped` /
`count_facts_by_subject_scoped` + `offset` on three list methods; `count_entities` /
`count_entities_by_kind` / `count_documents` / `count_memories_scoped` + `offset` on four more.
Deliverables 1 (provenance), 2 (returned/total/cursor) and 5 (diary-scoped conflicts) are three of
this lane's eight and are the two the brief's move 5 names by defect.

Same shape, smaller: **daemon-jobs** needs five storage additions in wave 4 that wave 2 does not
plan (`with_tx_res` with unwind rollback, `in_transaction`, `open_reader`, `rollback_if_open`,
`derivation_cache_available`) plus `conn` becoming `pub(crate)`; **reasoning-layer** needs S-1,
S-3, S-5..S-8. Storage-journal's spec is scoped, coherent and complete for its *own* deliverables
and does not know about any of these.

### S3 — the v9 fixture must be captured before `CURRENT_VERSION` changes, and nothing in the wave plan says so

storage-journal OQ-5: "After the bump the tree cannot produce a v9 store, and the migration's
entire test suite depends on one. This is the single sequencing mistake in this lane that is
expensive to undo, so it is implementation step zero." Tests 30–41 (twelve migration tests, the
clamp, the collapse, the orphan case, `bench personal` unchanged) all rest on
`crates/engram-storage/tests/fixtures/v9_store.sql`, which must be generated by an unmodified
binary. The orphan row additionally cannot be seeded through the CLI at all (the FK cascade takes
it) and needs a hand-written `PRAGMA foreign_keys=OFF` block in the dump.

Wave 2 should be re-stated as: **step 0, capture the fixture; step 1, everything else.**

### S4 — `output.rs`'s types must be ruled before wave 2 writes them, not at the wave-3 gate

See M4. contract-codegen ships `Listing`/`Scope`/`Verdict`/`Absence` in wave 2 "so wave 3 has
something to call"; read-surfaces needs three of the four changed. A wave-2 file written to the
wrong shape is a wave-3 rewrite of another lane's file.

### S5 — the `is_open_fact()` sweep is the only named integration gate, and four other gates have no owner

R15/CD-2 names the wave-3 sweep gate (storage's fourteen `sqlite.rs` sites + read-surfaces'
`facts.rs:123,156,157`, `recall.rs:310`) and gives it to the lead. Four more gates exist in the
specs with no owner:

| gate | un-ignores | named in |
|---|---|---|
| wave-3 **producer** gate (`respond.rs` patches 7–10) | codegen `all_four_producers_agree`, `suggested_action_vocabulary`; read-surfaces §5.4.5/5.4.6 | codegen §6.10 |
| wave-3 **mutator** gate (`forget`/`edit` handles) | codegen `unknown_id_exits_3_on_every_mutator`; read-surfaces §5.1.2/5.1.3 | codegen §6.10, read-surfaces §5 |
| wave-3 **listing** gate (`output::Listing` adoption) | codegen `listing_carries_returned_total_count`, `documented_data_fields_are_present`; read-surfaces §5.2.6's `memory list` row | codegen §6.10 |
| wave-5/6 gate | fact-semantics tests 38–39, write-surfaces 20/26/33/38, reasoning-layer 32 and (under S-1 refusal) 18/23/24/27/29 | five specs |

CC-4's pin was "a lane may commit with `#[ignore]`d tests only if the ignored list appears in its
lane report **and in a wave-integration gate that un-ignores them**". Six lanes now satisfy the
first half. One gate satisfies the second.

### S6 — storage-journal's wave-2 sweep is inert, and its wave-3 half is where the divergence appears

storage OQ-8 states it: until fact-semantics emits `close_reason='stopped'`, the fourteen
rewritten `sqlite.rs` predicates are provably equivalent to the old ones. The risk is the inverse
— fact-semantics lands and read-surfaces' four sites slip — and then storage says a medication is
stopped while `facts list` and `recall` say it is current. That is the gate of S5 row 1 and it is
correctly owned; the point here is that **it is a hard ordering constraint, not a cleanup**:
fact-semantics must not land `stopped` before read-surfaces' four sites.

### S7 — `--facts-json` provenance changes classifier behaviour that fact-semantics has not modelled

write-surfaces R3/R5a split a mixed `--facts-json --extract-facts` run into two sequential
batches so the caller's triples are stamped `UserStated`. Its own risk 2 is precise: "an extracted
triple that contradicts a caller triple now meets a `UserStated` incumbent instead of racing it
inside one batch. That is the intended direction … but it is a behaviour change in a path
fact-semantics' classifier owns." Fact-semantics §2.3.1 step 7's `correction_by_user` rule fires
on exactly this shape and its spec still says the rule "does not fire today at all". W6 makes it
live; the rule has never run against a real input and its test constructs source kinds directly.

### S8 — `ensure_jobs_schema` mutates rows at every store open, in a table storage-journal's v10 may also touch

daemon-jobs §4.1 issues twelve `ALTER`s, two indexes, a destructive `UPDATE` (abandoning duplicate
live jobs) and a partial unique index, from `ensure_jobs_schema()`, at store-open time, outside
migration versioning. Its own OQ-2/OQ-3/OQ-10 ask whether storage-journal's v10 step does the
same. If both run, "two idempotent functions produce two different tables depending on which ran
first, and the collapse (which mutates rows) runs twice with different survivors". Additionally
the collapse writes `status='abandoned'` with no event — a projection mutation with no journal
append, which invariant 1 forbids for every other table.

### S9 — reasoning-layer needs edits in four other lanes' wave-2 files

`EventKind` (+3 variants, `engram-core`, storage-journal), `HandleKind` (+`Lesson`, same),
`admin rebuild`'s dispatcher (+3 arms, storage-journal), `open_intervals`' signature (storage,
frozen by two other specs), `remember::write_memory` (write-surfaces, W-1), and the scheduler's
kind allowlist (daemon, D-5 — without which D-3 is inert and every non-`--inline` `state reflect`
queues forever). None is hard; all six are in waves 2–4 for a wave-5 lane, so they must be
requested now or the lane ships `--inline`-only.

---

## 4. Duplicated work

### W1 — `facts resolve`, twice (see M1)

read-surfaces wave 3 and daemon-jobs wave 4 both rewrite `facts.rs:451-517`, against different
storage APIs, with different event-append responsibilities.

### W2 — the `/v1/recall` conflicts array, three times

contract-codegen patch 10 ("**insert**, do not rewrite: that range … has no `conflicts` key at
all today"), daemon-jobs D6 (§3.8, with `surface_conflicts_for`), read-surfaces CLP-8 ("noted, not
requested for wave 3"). Verified: `serve.rs` contains zero occurrences of `conflicts`. Three lanes
have written the same insertion into three documents; exactly one should own it (daemon-jobs, its
file, wave 4) and the other two should record the dependency.

### W3 — the cursor codec, three times (M11); `Provenance`, twice (M12)

### W4 — `fact.interval_closed` and `decision.recorded` appended twice per operation

Storage appends inside the method; daemon appends around it (M1). write-surfaces caught the same
class for `memory.*` and escalated it as its Q9 ("Both landing as written = duplicate events and
double-closed intervals; either alone = the other lane's plan is fiction") and pre-emptively
rewrote its §2.2/§2.5 to assume storage appends. daemon-jobs did not, and read-surfaces did not
need to. **One rule settles all three lanes** (D7).

### W5 — the `--store` flag, two mechanisms

daemon-jobs §8.3 implements it as `env::set_var("ENGRAM_STORE", …)` at the top of `dispatch`
(needed anyway for the `/v1/ingest` subprocess), and §10.1 *also* asks storage-journal for an
optional `paths::set_store_override(PathBuf)` `OnceLock`. Pick one as the mechanism and one as the
child-process export; shipping both means two sources for the same path.

### W6 — the safety gate, three call sites, one owner

fact-semantics §2.3.1 step 9 (`safety_gate` in the classifier), read-surfaces §3.4.9 (replacing
the inline defence at `facts.rs:581-593` with a call), daemon-jobs §5.2 step 4b (an independent
`may_close` gate before mutating). The last is not duplication — it is a second, coarser gate at a
different layer — but daemon calls `may_close` directly while everyone else calls `safety_gate`,
and `may_close` alone does not cover the `Duplicate` downgrade that IN-19/fact-semantics §2.3.1
insists on. Worth one sentence in interfaces saying which funnel applies where.

### W7 — `agent-info` / `skill.md` documentation requests from five lanes

CLP-5 (read-surfaces), XC-3 (write-surfaces), C-3/C-4/C-6 (reasoning-layer), §10.2 (daemon),
plus codegen's own registry. All correctly routed to contract-codegen. The gap is the *channel*:
codegen's OQ 10 asks who tells it when a field appears, and the answer needs to be R15's patch
table, not five specs each hoping.

---

## 5. Contradictions with the brief's hard constraints

### C1 — constraint 3 ("existing JSON fields keep their names and types") vs eight deliberate value changes in five lanes

The brief's constraint 3 is unchanged since v1. CC-3 asked for it to be amended; it was not. The
changes now sitting in five specs, each individually argued and correct:

| change | lane | field |
|---|---|---|
| `suggested_action` value set narrows on safety pairs | fact-semantics / R7 | value |
| `fact_type` stops being `profile_slot` for safety relations | fact-semantics | value |
| `arity` flips `single`→`multi` for `allergic_to` | fact-semantics | value |
| `resolved_by_rule` / `resolution` histogram keys change for three relations | fact-semantics | key set |
| conflict-entry `status` widens from `superseded\|contested` to six values | read-surfaces | value set |
| `error.code` `storage_error` is emitted for no case | contract-codegen | value |
| `export.version` 2 → 3 | write-surfaces | value |
| `forget` on an unknown id: exit 0 → exit 3 | write-surfaces | exit code |

Every one is deliberate and defensible. The problem is that constraint 3 is what a lane is judged
against, and five lanes are each independently ruling that "names and types" does not cover
values. That is a brief amendment plus one CHANGELOG heading ("BREAKING FOR AGENTS"), which three
specs already write independently (codegen §5.1, write-surfaces XL-2, read-surfaces CLP-6).

### C2 — constraint 7 (never touch another lane's files) has three live exceptions the patch channel does not cover

1. **Two owners of `crates/engram-core/src/error.rs`** (V10).
2. **daemon-jobs requests a whole new file inside `engram-storage`** (`src/jobs.rs`, ~350 lines,
   §9.1) — a scoped ownership exception, not a one-line patch. Its fallback (storage implements
   §4.2 in wave 2) "costs a wave and produces an API designed by a lane that has no caller for
   it", which is a fair argument and still needs a ruling.
3. **`sqlite.rs:26` `conn` → `pub(crate)`** is requested independently by storage-journal (§2.2,
   for its own modules) and daemon-jobs (§10.1). Harmless duplicate; noted so it is applied once.

### C3 — constraint 5's green gate vs 30-odd `#[ignore]`d tests behind four unowned gates

See S5. Constraint 5 is satisfiable per-lane and the guarantees are not. The two that matter most:
move 3's headline (a caller decision survives re-derivation — fact-semantics 38/39, write-surfaces
20) and move 5's headline (a recall id is accepted by a mutator — read-surfaces §5.1.2) are both
`#[ignore]`d in every lane that touches them.

### C4 — constraint 4's migration contract does not cover v11

reasoning-layer S-1 bumps `CURRENT_VERSION` to 11 for the `lessons` table. Interfaces §2.1 and
constraint 4 bind v9→v10 only, including the `VACUUM INTO` backup rule. reasoning-layer X-4 asks
for the extension and proposes the right rule (additive-only, `CREATE TABLE IF NOT EXISTS`, no
backfill, therefore no backup). Until it lands, a lane is bumping the schema version outside the
contract that governs schema bumps.

### C5 — constraint 1 vs `env::set_var` and vs two HOME-setting test harnesses

`--store` via `env::set_var` (daemon §8.3) makes store isolation depend on one call at the top of
`dispatch` executing before any path resolution, in a process whose tokio workers already exist.
It is defended and tested (`ingest_subprocess_inherits_the_store` is the right test) and it is
still the one place in the build where constraint 1 rests on ordering rather than on a type. The
two HOME-setting harnesses are V11.

### C6 — CC-1, CC-2, CC-6, CC-7 from v1 are **closed**

The brief's constraint 1 now names `ENGRAM_STORE`; constraint 4 now specifies `VACUUM INTO` with a
timestamp and says the v8 precedent does not exist; the lane table's interval-types entry is
superseded by R1 + interfaces §0; the two already-fixed defects (agent-info before `AppContext`,
`ENGRAM_STORE` in `paths.rs`) are named in the brief's "already landed" list and in interfaces §7.
Recorded so nobody re-raises them.

---

## 6. R7 conformance audit — what spec-fact-semantics.md must change

R7 in full: *vocabulary stays `keep-both | supersede-old | requires_context`; safety pairs ALWAYS
`requires_context`, never `supersede-old`; non-safety single-valued pairs that queued only because
autonomy was `strict` (classifier verdict was `SupersedeOld`) may honestly suggest
`supersede-old`; set-valued non-safety different members never queue at all; the blanket removal
in §1.4 is overruled; contract-codegen generates this enum.*

Eleven edits, in document order. Nothing else in the spec depends on the blanket rule.

| # | site | current text | required |
|---|---|---|---|
| 1 | §1.1 item 4 (the section R7 names as "§1.4") | "the suggestion vocabulary shrinks to `keep-both` \| `requires_context`. `supersede-old` is never *suggested* again" | "the suggestion vocabulary stays `keep-both \| supersede-old \| requires_context`. `supersede-old` is never suggested on a SAFETY pair, and never as a laundering of classifier uncertainty; it survives only where the classifier reached `SupersedeOld` and `--autonomy strict` is the sole reason the pair queued (R7)" |
| 2 | §1.1 item 4, "Kills `fact_write.rs:1209-1213`" | unchanged claim | keep — the `detector == "slot"` hard-coded suggestion still dies; the replacement is `suggested_action()`, which may return `supersede-old` in the narrow case |
| 3 | §2.3, doc comment on `pub fn suggested_action` | "What the CALLER should be told to do. Never `supersede-old`." | "…Never `supersede-old` on a safety pair, and never when the classifier could not order the values (R7)." |
| 4 | §2.3.1's `suggested_action` table (3 arms) | safety → `requires_context`; four rules → `requires_context`; otherwise → `keep-both` | four arms, in this order: (1) either side safety → `requires_context`; (2) rule ∈ {`same_source_contradiction`, `comparable_values`, `paraphrase_slot`, `immutable_attribute_disagreement`} → `requires_context`; (3) `v.escalated_from == Some(SupersedeOld)` **and** neither side safety **and** the relation is single-valued → `supersede-old`; (4) otherwise → `keep-both` |
| 5 | §2.3, `struct Verdict` | `{ class, rule }` | `{ class, rule, escalated_from: Option<ConflictClass> }` — **R7's honest case is not computable without it**; §2.3.3 step 9.5 currently discards the pre-escalation class when it turns a `closes` entry into a `queue` entry. `safety_gate` sets it too, so arm (1) and arm (3) can never both match |
| 6 | §2.3.3 step 9.5 (`Autonomy::Strict`) | "every `closes` entry whose authority is `SuccessorInterval` or `Heuristic` becomes a `queue` entry instead" | same, plus: the resulting `PlannedConflict.verdict` carries `escalated_from: Some(SupersedeOld \| SupersedeNew)`; `queue_everything` escalations carry `escalated_from: None`, because `ENGRAM_FACT_QUEUE_ALL` is a test seam and must not change advice |
| 7 | §2.3.1 step 9 (`safety_gate`) | downgrades to `Queue("safety_slot")` | unchanged behaviour; add "sets `escalated_from`, which arm (1) of `suggested_action` then never reads — safety wins first, unconditionally" |
| 8 | §2.4, `queue_conflict` bullet 1 | "`suggested` comes from `suggested_action`, never from `detector == "slot"`" | unchanged and correct — the value now comes from the four-arm table |
| 9 | §4.2 "JSON fields" bullet | "`suggested_action` … gains one new value (`requires_context`) while **losing one it emitted** (`supersede-old`)" | "gains one new value (`requires_context`); `supersede-old` is retained but narrowed — it can no longer appear on a safety pair. The vocabulary is generated by contract-codegen from `engram_contract::vocabulary("suggested_action")` (R7)" |
| 10 | §5 test 17 `suggested_action_is_never_supersede_old` | property test over every emittable `Verdict` | split into `suggested_action_is_never_supersede_old_on_a_safety_pair` (property test, unchanged strength, safety-scoped) **plus** a new positive test `a_strict_escalated_slot_update_still_suggests_supersede_old`, which is the only test that proves R7's honest case fires. Without the second test the blanket rule can be reintroduced silently and every test stays green |
| 11 | §7 risk 9 | "**Open question for the lead**: narrow the change to safety pairs …, or keep the blanket rule? The spec currently implements the blanket rule." | replace with "**RULED (R7)**: narrowed to safety pairs plus the classifier-uncertainty rules; the blanket rule is overruled. `supersede-old` remains an accepted `--action` value forever." An open question that has been answered is how an implementer re-litigates it |

Two consistency checks that pass and should be left alone: §2.3.1's `allowed_actions` is
`["keep-both", "supersede-old", "reject-new"]` for **every** pair including safety ones — that is
correct under R7 (a caller decision *is* `CloseAuthority::Caller`, so permission is not narrowed,
only advice), and it matches contract-codegen's `ResolveAction::ALL` and read-surfaces §3.3.6's
note that `allowed_actions[0]` must stay `keep-both`. §4.1 Phase 2's drain table row for a dose
change (`safety_member_change` → gate → `safety_slot`, `requires_context`) is already R7-correct.

Cross-lane note: read-surfaces §3.3.6 and contract-codegen §4.1 both implement R7 correctly and
both explicitly record that spec-fact-semantics is stale ("R7 is the binding text here and it
overrides `spec-fact-semantics.md` §1.4"). read-surfaces additionally ships a **read-side** R7
guard (`suggested_action_for(row_suggested, safety)`) for the 951 pre-wave-3 queue rows, with
`metadata.suggested_action_overridden` so the guard cannot mask a write-side regression. That is
the right shape and it should survive fact-semantics' revision.

---

## 7. Consolidated interface delta — what interfaces.md v2 still misses

Exact signatures. `⚠` marks an item where two specs disagree and the lead must choose rather than
merge. Ordered by what blocks the earliest wave.

### 7.1 Blocking wave 2

**D15 ⚠ — the semantic-key home (OQ-1, amends R9).** Recommended ruling:

```rust
// engram_core::keys  (NEW; engram-core takes no new dependency — uuid carries v5 at Cargo.toml:81)
pub const SEMANTIC_KEY_NAMESPACE: Uuid = uuid::uuid!("6d1b9d3a-4f0e-5a7c-9b21-8f1c0d6e4a55");
pub const SEMANTIC_KEY_VERSION: &str = "semantic_key@1";
pub const CONTENT_HASH_NAMESPACE: Uuid = uuid::uuid!("2f7c5e88-1a34-5c62-8d90-3b4e7a1f6c02");
/// hex(uuid5(NS, min(a,b) ++ 0x1f ++ max(a,b))) over the two facts' `object_norm`s
/// — NOT their member identities: the migration has only `object_norm`.
pub fn object_pair_hash(a_norm: &str, b_norm: &str) -> String;
pub fn semantic_key_encode(subject_norm: &str, relation: &str, pair_hash: &str) -> String;
pub fn semantic_key_decode(s: &str) -> Option<(String, String, String)>;
pub fn content_hash(content: &str) -> String;
```
`engram_graph::SemanticKey` keeps its struct and `for_pair(&FactView, &FactView)` and delegates.
blake3 stays rejected (V2). One encoding, one namespace, no ownership change to the typed API.

**D1 ⚠ — `crates/engram-cli/src/respond.rs`, the full surface (R12 + M3).** Owner
contract-codegen, wave 2. `resolve_payload` and `resolve_block` are **not** names.

```rust
pub const PROGRAM: &str = "engram";
pub enum ResolveAction { KeepBoth, SupersedeOld, RejectNew }   // the ONE definition
impl ResolveAction {
    pub fn as_str(self) -> &'static str;                        // keep-both|supersede-old|reject-new
    pub fn parse(s: &str) -> Option<Self>;
    pub const ALL: [ResolveAction; 3];                          // KeepBoth first: the safe blind default
}
pub struct ResolvePayload { pub program: &'static str, pub args: Vec<String>,
                            pub allowed_actions: Vec<&'static str> }
pub fn resolve_conflict(conflict_id: Uuid, allowed_actions: &[&'static str]) -> ResolvePayload;
pub fn retry_with_handle(command_path: &str, correct_handle: &str, extra: &[&str]) -> ResolvePayload;
pub fn resolve_json(p: &ResolvePayload) -> serde_json::Value;
pub fn resolve_command_string(p: &ResolvePayload) -> String;    // one command, no pipe, shell-quoted
pub fn attach(obj: &mut serde_json::Map<String, serde_json::Value>, p: &ResolvePayload);

// The envelope half (daemon-jobs §3.8). RULING NEEDED: here, or in output.rs.
pub struct ConflictInput<'a> { pub id: Uuid, pub new: &'a Fact, pub old: &'a Fact,
                               pub detector: &'a str, pub suggested: &'static str,
                               pub allowed: &'static [&'static str], pub status: &'static str }
pub fn success(data: serde_json::Value, meta: crate::output::Metadata) -> serde_json::Value;
pub fn error(err: &crate::error::CliError) -> serde_json::Value;
pub fn listing(items: Vec<serde_json::Value>, l: &crate::output::Listing) -> serde_json::Value;
pub fn conflicts(rows: &[ConflictInput<'_>]) -> Vec<serde_json::Value>;
```

**D2 ⚠ — `output.rs` envelope types, final field types (M4).** Recommended, taking the consumer's
side on every disagreement:

```rust
pub struct Listing { pub returned: usize, pub total: i64, pub cursor: Option<String> }
impl Listing { pub fn full(items: usize) -> Self;
               pub fn page(returned: usize, total: i64, cursor: Option<String>) -> Self;
               pub fn attach(&self, obj: &mut serde_json::Map<String, serde_json::Value>); }

pub struct Scope { pub kbs: Vec<String>, pub diary: String,
                   /// None on surfaces that score no chunks; the KEY IS OMITTED, never 0.
                   pub chunks_considered: Option<usize>,
                   /// KbScopeOrigin::as_str() — "flag" | "config" | "all_kbs". No second vocabulary.
                   pub origin: &'static str }

pub struct Absence { pub top_score: Option<f32>,
                     /// None until a real cutoff rejects a real candidate. Never a fabricated 0.0.
                     pub threshold: Option<f32>,
                     pub searched_kbs: Vec<String>, pub searched_chunks: usize,
                     /// no_chunks_in_scope | filtered_by_window | filtered_by_diary
                     /// | no_candidate_matched | trimmed_by_layer_budget
                     /// | subject_not_found        (reasoning-layer)
                     pub reason: &'static str,
                     #[serde(default, skip_serializing_if = "Vec::is_empty")]
                     pub nearest_subjects: Vec<NearestSubject> }
pub struct NearestSubject { pub subject: String, pub score: f64 }
pub enum Verdict { Results, NoMatch(Absence) }
```
Plus a ruling on `metadata.scope.diary_applicable`, `metadata.command_alias` and
`metadata.suggested_action_overridden` (bless or drop — read-surfaces invents all three).

**D3 — `Fact` gains `kb` (M7).**
```rust
// engram_core::types::Fact, additive, appended after `confirmations`
#[serde(default)] pub kb: String,   // "" on a row deserialized from a pre-v10 export
```
Every `SELECT` that feeds `parse_fact_row` gains the column (storage-journal §3.2 already
re-pins every caller offset for the three close columns; this is a fourth on the same pass). It
also removes the `insert_fact` `COALESCE((SELECT kb FROM memories …))` rebuild hazard storage
names in §2.12.

**D7 — who appends events, one rule (M1, M10, W4; settles write-surfaces Q9 and daemon OQ-9).**

> **Storage appends every event for the projection it owns.** No CLI lane appends `memory.*`,
> `fact.*`, `derivation.*` or `decision.*` directly; a lane calls the storage method and reads
> back what it did. `job.*` and `compile.ran` are daemon-jobs' and are appended inside the
> `Core::write` closure that performs the effect. `lesson.*` are reasoning-layer's, appended
> inside `upsert_lesson` / `set_lesson_status`.

```rust
impl SqliteStore {
    /// Appends `decision.recorded` (full R4 payload) AND inserts the projection row,
    /// both in the caller's open transaction. The event id is internal.
    #[allow(clippy::too_many_arguments)]
    pub fn record_decision(&self, kb: &str, diary: &str, key_encoded: &str, verdict: &str,
                           authority: &str, actor: &str, note: Option<&str>,
                           conflict_id: Option<Uuid>, key_version: &str)
        -> Result<Uuid, StorageError>;
}
```
Interfaces §3.4's "fields as DecisionRow minus id/event_id" is the right intent and the wrong
words; replace with the signature above.

**D5 ⚠ — memory-scoped fact closure, split, with the reason/authority table pinned (M2, M5).**
```rust
impl SqliteStore {
    /// Close every OPEN interval derived from `memory_id`; one `fact.interval_closed`
    /// per fact, in the caller's transaction; returns the ids closed.
    /// Does NOT touch `fact_extraction_log`. Must NOT filter on `memories.deleted = 0`
    /// (forget calls it after the soft delete, in the same transaction).
    pub fn close_facts_for_memory(&self, memory_id: Uuid, at: DateTime<Utc>,
                                  reason: CloseReason, authority: &CloseAuthority, actor: &str)
        -> Result<Vec<Uuid>, StorageError>;
    /// Separate, so `edit` can close facts WITHOUT clearing the marker (write-surfaces E7a).
    pub fn clear_extraction_marker(&self, memory_id: Uuid) -> Result<bool, StorageError>;
    /// The old delete body, kept for `hard_delete_memory` only — ingest rollback.
    pub fn purge_facts_for_memory(&self, memory_id: Uuid) -> Result<usize, StorageError>;
    pub fn memory_scope_including_deleted(&self, id: Uuid)
        -> Result<Option<(String, String, bool)>, StorageError>;   // (kb, diary, deleted)
    pub fn soft_delete_memory(&self, memory_id: Uuid)
        -> Result<Option<RetractOutcome>, StorageError>;
}
pub struct RetractOutcome { pub retracted: bool, pub closed: Vec<Uuid> }
```
Close reason/authority, pinned once:

| path | `close_reason` | `close_authority` |
|---|---|---|
| `edit --content` | `corrected` | `caller` (actor in the event's `actor` column) |
| `forget` | `retracted_source` | `caller` |
| `repair_integrity` (memory already deleted) | `retracted_source` | `explicit_end` |
| `facts resolve --action supersede-old\|reject-new` | `superseded` | `caller` |
| write-path supersession | `superseded` | `successor_interval` or `heuristic` |
| extractor said the state ended | `stopped` | `explicit_end` |

`supersede_fact_with(resolution = "caller")` therefore maps to `superseded`/`caller`, **not**
`corrected` (storage §2.8's table changes on one row). `"caller:<actor>"` is not a legal wire
value; `CloseAuthority::as_str()` (landed) is the only source, including `successor_interval`.

**D6 — clearing an extraction marker must be rebuildable (M2b).** Two options; the second is
cheaper and needs no new `EventKind`:
1. `derivation.ran` payload gains `"invalidated": true` on a re-append, or
2. **recommended** — interfaces §2 invariant 3's rebuild dispatcher gains one rule:
   `memory.revised` deletes the `fact_extraction_log` marker for its `memory_id`. `edit --content`
   always appends `memory.revised`, so the marker state becomes a pure function of the journal.
   State it in the rebuild table beside `derivation.ran`, or `admin rebuild` silently un-pends
   every edited memory.

**D8 — `facts resolve`, one implementation (M1, W1).**
```rust
impl SqliteStore {
    /// Claim + decision + event in ONE transaction. `Ok(None)` = the claim was lost
    /// and is the SOLE lost-claim signal; there is no second `claim_*` entry point.
    #[allow(clippy::too_many_arguments)]
    pub fn resolve_conflict_with_decision(&self, conflict_id: Uuid, kb: &str, diary: &str,
        key_encoded: &str, verdict: &str, authority: &str, actor: &str, note: Option<&str>,
        key_version: &str) -> Result<Option<Uuid>, StorageError>;
}
```
Ruling: read-surfaces calls it in wave 3. `Core::resolve_conflict` in wave 4 wraps **this**
method, adds the cycle guard and the `may_close` gate, and appends nothing. `resolve_fact_conflict`
stays deprecated-by-doc-comment and gains no new caller.

**D16 — restate R10 for fact-semantics (V5).** `insert_fact_conflict` returns `InsertOutcome`;
`DuplicatePair{existing}` and `DuplicateKey{existing}` are distinguished; the applier's
`deduplicated: true` response is built only from `DuplicateKey`.

**D18 — `Verdict` carries the pre-escalation class (V1/R7).**
```rust
pub struct Verdict { pub class: ConflictClass, pub rule: &'static str,
                     /// Set by `safety_gate` and by the planner's strict-autonomy step.
                     /// R7's `supersede-old` case is not computable without it.
                     pub escalated_from: Option<ConflictClass> }
```

**D19 — close vocabulary has one source.** `close_reason` ∈ `CloseReason::as_str()`,
`close_authority` ∈ `CloseAuthority::as_str()` (`superseded|stopped|corrected|retracted_source`;
`explicit_end|caller|replayed_caller|successor_interval|heuristic`). Contract-codegen's
`vocabulary()` table already reads them; interfaces should say the enums are normative.

**D13 — `Interval::is_open()` and `is_open_fact()` must agree (M13).** Pin one:
`Interval::of` sets `until = valid_until.or(superseded_at).or(if superseded_by.is_some() {
Some(created_at) } else { None })`, so a row with a successor is never open under either
predicate.

**D20 — `ErrorCode` wire strings, and the daemon's status table (V3-adjacent).** Contract-codegen
patch 2a renames `Config`/`Transient` `as_str()` and serde to `config_error`/`transient_error`
(constraint 3). daemon-jobs §3.4's `http_status` matches on `"config"` and would then fall to
`_ => 500`. Both spellings must be decided in one place, and daemon's table updated in the same
patch:
```rust
"bad_input" | "malformed_json" | "wrong_id_kind" | "config_error" => 400,
"not_found" => 404, "conflict_already_resolved" => 409, "rate_limited" => 429,
"sqlite_busy" => 503, _ => 500,
```
Also pin `ErrorCode::retryable()` and `ErrorCode::ALL` (codegen patch 2b) as part of the enum, not
as a lane's local addition.

### 7.2 Blocking wave 3

**D4 ⚠ — `open_intervals` / `facts_for_relation` final signatures (M-, S2, reasoning S-5/S-8).**
Two other specs froze `&str`; reasoning-layer's default invocation cannot use them.
```rust
impl SqliteStore {
    pub fn facts_for_relation(&self, kb: Option<&str>, diary: Option<&str>, subject_norm: &str,
                              relation: &str, include_closed: bool)
        -> Result<Vec<Fact>, StorageError>;
    pub fn open_intervals(&self, kb: Option<&str>, diary: Option<&str>, subject_norm: &str,
                          relation: Option<&str>, include_closed: bool)
        -> Result<Vec<Fact>, StorageError>;
}
```
`None` = every KB / every diary, matching `list_facts_by_subject_scoped`. Refusing this is legal
and costs an N×M fan-out in `state`; refusing it *silently* is what produces `WHERE kb = '*'`.

**D17 — the read-method acceptance list (S2).** These must enter storage-journal's deliverable
list or the three read deliverables slip. Verbatim from read-surfaces §6.2 and reasoning-layer
§6.4, deduplicated:
```rust
impl SqliteStore {
    pub fn memory_ids_for_chunks(&self, chunk_ids: &[Uuid]) -> Result<HashMap<Uuid, Uuid>, StorageError>;
    pub fn facts_matching_terms(&self, kb: Option<&str>, diary: Option<&str>, terms: &[String],
                                limit: usize) -> Result<Vec<Fact>, StorageError>;
    pub fn open_conflicts_matching_terms(&self, kb: Option<&str>, diary: Option<&str>,
        terms: &[String], limit: usize) -> Result<Vec<(FactConflictRow, Fact, Fact)>, StorageError>;
    pub fn count_open_conflicts_matching_terms(&self, kb: Option<&str>, diary: Option<&str>,
        terms: &[String]) -> Result<i64, StorageError>;
    pub fn list_recent_conflicts(&self, diary: Option<&str>, kb: Option<&str>, limit: usize,
                                 offset: usize) -> Result<Vec<(Fact, Fact)>, StorageError>;
    pub fn count_recent_conflicts(&self, diary: Option<&str>, kb: Option<&str>) -> Result<i64, StorageError>;
    pub fn count_facts_scoped(&self, diary: Option<&str>, kb: Option<&str>, include_superseded: bool)
        -> Result<i64, StorageError>;
    pub fn count_facts_by_subject_scoped(&self, subject_norm: &str, diary: Option<&str>,
        kb: Option<&str>, include_superseded: bool) -> Result<i64, StorageError>;
    pub fn count_entities(&self, kb: Option<&str>, min_mentions: u32) -> Result<i64, StorageError>;
    pub fn count_entities_by_kind(&self, kb: Option<&str>, kind: &str, min_mentions: u32) -> Result<i64, StorageError>;
    pub fn count_documents(&self, kb: Option<&str>) -> Result<i64, StorageError>;
    pub fn count_memories_scoped(&self, diary: Option<&str>, kb: Option<&str>) -> Result<i64, StorageError>;
    pub fn chunks_for_memory(&self, memory_id: Uuid) -> Result<Vec<ChunkRecord>, StorageError>;
    pub fn distinct_fact_subjects(&self, diary: Option<&str>, kb: Option<&str>) -> Result<Vec<String>, StorageError>;
    pub fn events_since(&self, kinds: &[EventKind], from_seq: i64, limit: usize) -> Result<Vec<EventRow>, StorageError>;
    pub fn seq_at_or_after(&self, ts: DateTime<Utc>) -> Result<i64, StorageError>;
}
// plus `offset: usize` on list_facts_scoped, list_facts_by_subject_scoped (+ `limit`),
// list_open_fact_conflicts_with_facts, list_entities, list_entities_by_kind,
// list_documents, list_memories
// plus CREATE INDEX idx_facts_scope_open ON facts(kb, diary, valid_until, superseded_by)
//      — or whatever single index storage prefers; the requirement is the complexity class.
```

**D9 — `surface_conflicts`, one signature (M6).**
```rust
// crates/engram-cli/src/commands/recall.rs — read-surfaces owns it; daemon calls it
pub struct ConflictSurface { pub items: Vec<serde_json::Value>, pub total: i64 }
pub fn surface_conflicts(store: &SqliteStore, kb: Option<&str>, diary: Option<&str>,
                         query: &str, limit: usize) -> Result<ConflictSurface, CliError>;
```
`&SqliteStore`, not `&AppContext`, so the daemon can call it from a pooled reader. `diary` is not
optional-to-pass; the leak fix is the deliverable.

**D11 — cursors have one codec (M11).** `read_surface::{CursorKey, encode_cursor, decode_cursor,
next_cursor}` is the only implementation; `state` and `jobs list` call it. `lib.rs` gains
`pub mod read_surface;` in wave 3 (read-surfaces CLP-1).

**D12 — `Provenance`, one struct (M12).**
```rust
// crates/engram-cli/src/read_surface.rs
pub struct Provenance {
    pub chunk_id: Option<Uuid>, pub memory_id: Option<Uuid>, pub document_id: Option<Uuid>,
    pub kb: Option<String>, pub diary: Option<String>,
    pub derived_from: Option<&'static str>,          // "derived:wiki_page" | …
    pub observed_at: Option<DateTime<Utc>>,          // reasoning-layer
    pub snippet: Option<String>,                     // reasoning-layer, 240 chars
}
```

**D14 — the extractor id (M15).** `engram_storage::journal::{EXTRACTOR_FACT_LLM, EXTRACTOR_COMPILE}`
are the only spellings; write-surfaces' `EDIT_EXTRACTOR_ID` is `EXTRACTOR_FACT_LLM`.

**D10 — `AdminCommand`, one definition (M8).**
```rust
pub enum AdminCommand {
    /// Drop and replay the fact and adjudication projections from the event journal.
    Rebuild { #[arg(long)] kb: Option<String>,
              #[arg(long)] dry_run: bool,
              #[arg(long)] confirm: bool },
}
```

**D22 — invariant 3's scope, corrected (storage OQ-3).** Amend interfaces §2 invariant 3 to:
"`engram admin rebuild` drops and replays the **fact and adjudication** projections (`facts` minus
`embedding`/`embed_model`, `fact_conflict_queue`, `fact_decisions`, `derivations`,
`fact_extraction_log`) and reproduces byte-identical state for them. `memories`, `chunks` and the
compiler caches are out of scope and are named, with reasons, on every run." The released wording
of the guarantee depends on this.

**D23 — `delete_document` (M16).** Assign to storage-journal, in the same commit as the
`soft_delete_memory` rewrite: route the memory retraction through `soft_delete_memory` so a
`memory.retracted` event exists per memory, and add the path to §2.9b's table.

**D21 — `compile_jobs` DDL, one home (S8).** One `const JOBS_SCHEMA_SQL` with one owner; whoever
does not own it calls it. The duplicate-live-job collapse mutates rows and must run exactly once.

### 7.3 Blocking wave 4/5, decidable now

**D24 — `HandleKind` and `EventKind` closure rulings (M9).** Both enums are landed and pinned;
both need widening or an explicit refusal:
- `HandleKind` + `Lesson` (`lesson:`) — reasoning-layer X-2; refusal ⇒ bare uuid, no `handle`.
- `HandleKind` + `Wiki` — read-surfaces CC-R2b; refusal ⇒ `page_id`, no `handle`, and §4.1's
  "every row carries a handle" narrows to resolvable rows.
- `EventKind` + `LessonRecorded|LessonConfirmed|LessonRetracted` (→ `ALL: [_; 18]`), their three
  payloads in interfaces §2 (X-3), **and** the matching `admin rebuild` arms (S-4). All three or
  none: a kind that the rebuild dispatcher does not know is a hard error on every rebuild.

**D25 — migration v10→v11 contract (C4/X-4).** Extend interfaces §2.1: additive-only migrations
(`CREATE TABLE IF NOT EXISTS` + indexes, no backfill, no row rewrite) skip the `VACUUM INTO`
backup; any migration that rewrites existing rows takes it.

**D26 — `Core` and jobs (daemon §9.2).** interfaces §6 is one paragraph. It should carry
`Core::write`/`read`/`WriteCtx` and the five invariants; the job status vocabulary and the rule
that **`completed` means a full derived-layer rebuild** (`MEMORY_CONSTRAINS_DERIVED` depends on
it — a paused compile written as `completed` tells the store a deleted memory was compiled out
when it was not); `Core::resolve_conflict`'s claim-then-mutate order and
`ConflictAlreadyResolved`; and the HTTP-status ↔ `ErrorCode` table as the daemon half of R11.

**D27 — ownership additions.** `crates/engram-storage/src/jobs.rs` (daemon-jobs, scoped
exception — C2); `crates/engram-core/src/error.rs` (one owner — V10); `sqlite.rs:26` `conn` →
`pub(crate)` applied once; `CURRENT_VERSION` → `pub` (codegen patch 2f);
`crates/engram-storage` `[dev-dependencies] tempfile` (storage CLP-8);
`crates/engram-cli/Cargo.toml` `base64.workspace = true` (read-surfaces CLP-9).

**D28 — the FieldSpec channel (codegen OQ 10, CD-10).** A new response field is a row in
interfaces' patch table, applied by the lead in the same wave as the field. `no_undocumented_data_fields`
is the backstop, not the notification.

---

## 8. Readiness

Wave 2 = storage-journal + contract-codegen. Wave 3 = fact-semantics, read-surfaces,
write-surfaces. Wave 4 = daemon-jobs. Wave 5 = reasoning-layer.

| lane | verdict |
|---|---|
| storage-journal | **CONDITIONAL GO** — spec is v2-conformant and implementable; blocked only on D15 (OQ-1) for the migration's key encoding, and must capture the v9 fixture (S3) before touching `CURRENT_VERSION`. |
| contract-codegen | **CONDITIONAL GO** — everything but `output.rs` can start today; D2's field types (M4) and the `respond.rs` surface (D1/M3) must be ruled before those two files are written, and V10 must name one owner of `engram-core/src/error.rs`. |
| fact-semantics | **NO-GO** — the only spec never revised for interfaces v2; violates R1, R3, R5a, R7, R9, R10 and R15 (V1–V7) and would rebuild the two modules R1 moved. Needs one revision pass (§6 plus V2–V7) before wave 3. |
| read-surfaces | **NO-GO until accepted** — the spec is the most disciplined in the build, and deliverables 1, 2 and 5 do not compile until storage-journal accepts D17/SR-1…SR-9, which its wave-2 list does not contain; D2 and D9 must also land. |
| write-surfaces | **CONDITIONAL GO** — W6 (`--facts-json` → `UserStated`) and W1's parse-side mapping can start now; W3/W4/W5 are blocked on D5+D6+D7 (who appends, and the welded marker clear), which the lane correctly refuses to guess at. |
| daemon-jobs | **NO-GO for D5/D6, GO for the rest** — the HTTP parser, `Core`, jobs and `--store` are self-contained; `Core::resolve_conflict` must be re-specified as a wrapper (D8/M1) and `/v1/recall` waits on D1/D9. |
| reasoning-layer | **CONDITIONAL GO (wave 5)** — `engram state <subject>` is buildable once wave 2/3 land `Fact`'s close columns; the lessons/reflect half needs D24 (`EventKind`+`HandleKind`), S-1's v11 table and D25, and M14's `contains()` bug must be fixed before test 13b is written. |

Single highest-leverage action: rule D15 (unblocks wave 2 entirely), then D2 + D1 (unblocks
contract-codegen's two remaining files and three wave-3 consumers), then send spec-fact-semantics
back for one v2 conformance pass.

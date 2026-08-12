# Next cycle — step-level plan (post-0.4.1)

Execution plan for the cycle after 0.4.1. Ordering and constraints are
grounded in the ultra plan review (`review-nextcycle-ultra.md`) and the
gate-4 preconditions (`review-plangate4-codex.md`).

## Design constraints this plan is built around

1. **No implicit revival.** A correction (supersession, forget, prune) must
   survive the rebuild of the representation it was applied to. Document
   sync re-ingests a changed file under a new memory id; a correction pinned
   to the old physical row would silently vanish. Corrections need logical
   identity.
2. **Read surfaces must agree.** Closing a memory does not close the facts
   derived from it: `recall` would hide a claim while `state` kept asserting
   it. Any demotion must define its effect on every structured read — and
   closing facts is an authority act, so this is a design ruling, not an
   implementation detail.
3. **Multi-write commands are atomic.** Storage supports one
   rollback-capable transaction (`with_tx`); sagas with response-only
   recovery are not acceptable. Idempotency needs an operation id, not a
   timestamp comparison.
4. **The retraction quarantine is not a general invariant.** It fires only
   for memories that are deleted AND expired; ordinary ingested documents
   are never expired. Destructive sync cannot lean on it.
5. **The subtraction plan must be rebased before anything arms it.** As
   written it deletes `memory.valid_until` (load-bearing since 0.4.1) and
   `MemoryCommand::Add`. Arming requires the complete gate-4 precondition
   checklist, not a shortcut.
6. Two Tier-1 items (single-copy wire, abstention floor) are deferred, not
   done. Estimates live at the phase level; dry-run verification compares
   table/event/count equality, never SQLite file bytes.

## Execution order

### Phase 0 — now, in parallel

- **P0.1 Dogfooding (operator calendar time).** Day-1 baseline:
  `doctor --integrity --json` + `jobs list --json` counts saved to the field
  log. Day-3 and day-7 the same probes — drainage is a delta against
  baseline, not a single snapshot. Any wrong-feeling recall: capture query +
  response JSON immediately; file it as a field-log incident, do not hotfix
  mid-week. Day-7 human gate: operator declares pass/fail. Backup cleanup is
  a separate, operator-only step and requires one restore-verified v13
  backup first.
- **P0.2 Rebase `subtraction-plan.md` on 0.4.1.** Re-derive the delete list,
  the hot-SQL-caller inventory, and the retained-surface list against HEAD.
  Retain `valid_until` everywhere.

### Phase 1 — rulings (design only; the gate reviews the document)

One `interfaces.md` v3 addendum; every item is one written, binding
paragraph:

- D1 authority: `--facts-json` stamps `agent_extracted` by default; person
  authority is an explicit separate act, or deferred.
- D2 fact-processing receipt states, journalled, including complete-empty.
- D3 bare-`remember` contract (defer-loudly vs hard-fail — one choice).
- D4 edit parity (replacement facts on content edits).
- D5 `--offline` = global network deny, or rename to `--offline-embeddings`.
- D6 fact-model config out of `compiler.*`.
- D7 shell-extractor fallback: defer, with gate-4 H4 sandbox criteria as the
  reopening condition.
- D8 supersession semantics: memory-only or fact-closing (depends on D1);
  logical correction identity (constraint 1); the supersede/forget/prune
  chain matrix; same-successor retry vs competing successors; monotonic
  closure (never extend an earlier expiry); same-diary/same-KB
  authorization, cross-diary rejected before write.
- D9 one typed operation receipt for multi-write commands (supersede, sync),
  so neither invents a response-only partial state that response v2 later
  breaks.
- D10 documents/sync rulings: identity + sync-root ownership, path
  normalization and symlink policy, legacy NULL-hash adoption, add/move/
  missing branches, complete-scan requirement, mass-prune guard with bulk
  confirmation, v14 schema + export/import/rebuild parity for hash fields,
  pipeline fingerprint beside the content hash.
- E rulings: E1 `people`/entities (fact-coverage audit over the relationship
  memories before ruling; recommendation: thin fact-backed view, compiler
  graph deleted); E2 purge (defer only with a documented "forget/prune is
  not erase" warning, a risk owner, and a reopening trigger); E3 subtraction
  arming = the complete gate-4 checklist; E4 leave the stale 0.4.0 crates
  unyanked, with proactive yank triggers (corruption, security, migration
  hazard).

### Phase 2 — document identity + status (read-only)

- `content_hash` on `documents` under the v14 contract from D10 (keep
  `synced_at` only if D10 defines it as the atomic commit time of that
  hash), with export/import/rebuild parity tests.
- `ingest` stamps hashes; adoption path for the existing corpus per D10.
- `documents status --json` + `stale` on `documents list`. Ships alone.

### Phase 3 — supersession, atomic, on canonical `remember` only

- One transaction: successor write + predecessor close + typed link receipt
  commit or roll back together.
- Operation id for idempotency; same-pair retry returns the same receipt.
- Semantics per D8, fact handling per D1; the scope matrix (already-expired
  target, future-dated, retracted, dedup-to-target, dedup-to-expired,
  forget-of-successor) each pinned by a test.
- Contradiction-triggered supersession proposal: in scope or explicitly
  deferred — the D8 ruling says which.
- Not on `memory add` (scheduled for deletion; P0.2 confirms).
- Independent gate; ships 0.4.2.

### Phase 4 — subtraction, behind the complete gate-4 checklist

Authority/receipts (Phase 1), response v2, fact-owned identity, retained
commands + `people` decision (E1), export compatibility, the P4 SQL/event/
handle/lesson-twin inventory, calibration harness extracted from
engram-bench, old-store/journal/export migration proof. Two-step rollout:
quiescence release first, then the v14 destructive migration, rehearsed on a
store copy.

### Phase 5 — destructive sync: `ingest --sync` mutation + prune

After subtraction removes the wiki/entity/relation aggregates (or after a
general deleted-source aggregate quarantine exists — D10 decides which).
Reuses the existing atomic same-path re-ingest primitive rather than
composing public delete/re-ingest verbs. Combined gate with supersession:
supersede-then-sync, sync-then-supersede, concurrent sync×sync, crash
injection at each durable boundary.

### Phase 6 — score calibration + abstention

Last, on the final candidate vocabulary, per gate 4.

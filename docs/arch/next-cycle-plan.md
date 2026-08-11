# Next cycle — step-level plan (post-0.4.1)

REVISED 2026-08-11 after the ultra plan review (`review-nextcycle-ultra.md`):
NO-GO on the first ordering, GO on the sequence below. The five High findings
that forced the revision, in one line each:

1. **B×C resurrection**: supersession pinned to a physical memory row dies
   when sync re-ingests the file under a new id — corrections need LOGICAL
   identity that survives representation rebuilds, or sync silently revives
   superseded claims.
2. **The old order violated the inherited gate**: design (D) must come first
   because supersession semantics ARE an authority question (D1), and the
   subtraction plan is now stale — it deletes `memory.valid_until`, which
   0.4.1 made load-bearing.
3. **"Closing the memory IS the demotion" is false across structured reads**:
   facts derived from a superseded memory stay open, so recall would hide X
   while `state` keeps asserting X.
4. **The supersede saga wasn't atomic** — storage already supports one
   rollback-capable transaction; use it, plus an operation id for real
   idempotency.
5. **"Prune composes with the retraction quarantine automatically" was
   false**: that quarantine requires deleted AND expired; ordinary ingested
   docs have `valid_until = NULL`, so pruning them leaves aggregates live
   until compile.

## Execution order (revised)

### Phase 0 — now, in parallel

- **P0.1 Dogfooding (operator calendar time).** Day-1 baseline:
  `doctor --integrity --json` + `jobs list --json` counts SAVED to the field
  log. Day-3 and day-7 same probes — drainage is a delta against baseline,
  not a single snapshot. Any wrong-feeling recall: capture query + response
  JSON immediately; disposition rule: file it as a field-log incident, do not
  hotfix mid-week. Day-7 human gate: operator declares pass/fail.
  Backup cleanup is a SEPARATE, operator-only step and requires one
  restore-verified v13 backup to exist first.
- **P0.2 Rebase `subtraction-plan.md` on 0.4.1.** It predates expiry: it
  deletes `memory.valid_until` (now load-bearing) and `MemoryCommand::Add`
  (which B touches). Re-derive its line counts and its retained-surface list
  against HEAD before anything arms it.

### Phase 1 — expanded rulings (design only; gate reviews the document)

The old Track D, widened. Every item is one written, binding paragraph in an
`interfaces.md` v3 addendum:

- D1 authority (facts-json → agent_extracted default; person authority is an
  explicit act or deferred).
- D2 fact-processing receipt states, journalled, including complete-empty.
- D3 bare-`remember` contract (defer-loudly vs hard-fail — one choice).
- D4 edit parity (replacement facts on content edits).
- D5 `--offline` = global network deny, or rename to `--offline-embeddings`.
- D6 fact-model config out of `compiler.*`.
- D7 shell-extractor fallback: DEFER with gate-4 H4 criteria as reopening
  condition.
- **D8 (new) supersession semantics**: memory-only or fact-closing? (closing
  facts is an authority act → depends on D1); logical correction identity
  that survives re-ingestion (the B×C resurrection rule: no implicit
  revival); the supersede/forget/prune chain matrix; same-successor retry vs
  competing successors; monotonic closure (never extend an earlier expiry);
  same-diary/same-KB authorization (cross-diary rejected before write).
- **D9 (new) typed operation receipt** for multi-write commands (supersede,
  sync): one shape, ratified now so B and C do not each invent response-only
  partial states that response v2 must later break.
- **D10 (new) documents/sync rulings**: identity + sync-root ownership,
  path normalization/symlink policy, legacy NULL-hash adoption (no blessing
  of today's bytes sight-unseen), add/move/missing branches, complete-scan
  requirement, mass-prune guard with bulk confirmation, v14 schema +
  export/import/rebuild parity for the hash fields, pipeline fingerprint
  alongside the content hash.
- **E rulings folded in**: E1 people/entities (require a fact-coverage audit
  over the relationship memories before ruling); E2 purge (defer only WITH a
  documented "forget/prune is not erase" warning, a risk owner, and a
  reopening trigger); E3 subtraction arming = the COMPLETE gate-4
  precondition checklist, not a two-item shortcut; E4 leave 0.4.0 unyanked,
  with proactive yank triggers (corruption, security, migration hazard).

Codex gates the rulings document. GO unblocks phases 2–3.

### Phase 2 — C-safe: document identity + status (read-only)

- C-hash columns (`content_hash`, plus `synced_at` ONLY if D10 defines it as
  the atomic commit time of that hash — else drop it), under the v14
  contract D10 specified, with export/import/rebuild parity tests.
- `ingest` stamps hashes; adoption path for the existing corpus per D10.
- `documents status --json` + `stale` on `documents list`. Read-only, ships
  alone.

### Phase 3 — B: supersession, atomic, on canonical `remember` only

- One transaction (`with_tx`): successor write + predecessor close + typed
  link receipt commit or roll back together. No saga, no response-only
  recovery.
- Operation id for idempotency; same-pair retry returns the same receipt.
- Semantics exactly per D8 (including fact handling per D1); scope matrix
  (already-expired target, future-dated, retracted, dedup-to-target,
  dedup-to-expired, forget-of-successor) each pinned by a test.
- Contradiction-triggered supersession proposal (adopted in
  `field-report-deltas.md`): in scope here or explicitly deferred — say
  which in the D8 ruling.
- Not on `memory add` (subtraction deletes it; rebased plan P0.2 confirms).
- Independent gate; ships 0.4.2.

### Phase 4 — subtraction, only after the full gate-4 checklist

Authority/receipts (Phase 1), response v2, fact-owned identity, retained
commands + people decision (E1), export compatibility, the P4 SQL/event/
handle/lesson-twin inventory, calibration harness extracted from
engram-bench, old-store/journal/export migration proof. The rebased plan
from P0.2 is the working document.

### Phase 5 — C-destructive: `ingest --sync` mutation + prune

After subtraction removes the wiki/entity/relation aggregates (or, if it
slips, after a general deleted-source aggregate quarantine exists — D10
decides which). Reuses the existing atomic same-path re-ingest primitive
(`ingest.rs`) rather than composing public delete/re-ingest verbs. Combined
B×C gate: supersede-then-sync, sync-then-supersede, concurrent sync×sync,
crash injection at each durable boundary.

### Phase 6 — post-subtraction score calibration + abstention

Unchanged from gate 4: last, on the final candidate vocabulary.

## Standing corrections from the ultra review

- Stop saying "Tier 1 is done": two Tier-1 items (single-copy wire,
  abstention floor) are DEFERRED, not done, and remain on the board.
- B is not a half-day once atomicity, fact semantics, receipts and the gate
  are counted; C is not 2–3 days once migration, adoption, ownership and
  recovery are counted. Estimate at the phase level, not the flag level.
- Dry-run verification compares table/event/count equality, not a SQLite
  file hash (WAL makes byte-identity meaningless).

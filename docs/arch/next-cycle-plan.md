# Next cycle — step-level plan (post-0.4.1, 2026-08-11)

0.4.1 is live (crates.io + tap), the operator's store is migrated to v13, and
Tier 1 of `field-report-deltas.md` is done. This is the step-level
decomposition of what comes next, in execution order. Steps small enough that
each is one commit or one decision.

## Track A — prove 0.4.1 on the live store (calendar time, ~zero effort)

Tier 3 (subtraction) is gated on "0.4.x proven on a live store." The proof is
a week of ordinary use plus these checks:

- A1. Day 1 and day 7: `engram doctor --integrity --json` — expect `ok: true`,
  no orphan counts growing.
- A2. Day 3: `engram jobs list --json` — confirm fact-backfill jobs drain
  rather than accumulate (the 0.3.1 field report's silent-divergence class).
- A3. During the week: use `remember --valid-until` on anything with a real
  end date; note UX friction verbatim in the (local, not committed) field log.
- A4. Any recall that FEELS wrong → immediately capture query + response JSON
  into the field log. Fresh incidents are the fuel for the next field-report
  round.
- A5. Day 7: if zero integrity incidents → mark Tier 3 unblocked in HANDOFF;
  delete the manual pre-0.4.1 store backup and the July-era
  `db.sqlite.pre-v8/wave2/v7` backups inside the store dir (~1.5 GB back).

## Track B — `remember --supersedes <id>` (~0.5–1 day + gate)

Supersession rides the expiry machinery shipped in 0.4.1 — that is why expiry
went first. Closing the old memory's validity window IS the demotion; recall
already enforces it, reports it, and `--include-expired` already reaches
history.

- B1. Ruling first (one paragraph in `interfaces.md`): superseding = set the
  old memory's `valid_until = now` via the existing journalled
  `set_memory_valid_until`, plus a journal event carrying `{old, new}`.
  NO new column, NO new table (pre-subtraction discipline). Cross-KB
  supersession allowed with a warning field; self-supersession exit 3.
- B2. `cli.rs`: `--supersedes <HANDLE>` on `Remember` and `MemoryCommand::Add`
  (typed handle or bare uuid).
- B3. `remember.rs`: resolve via the existing handle resolver (`chunk:` follows
  to its parent, `resolved_from` reports it — same contract as edit/forget);
  `not_found`/`wrong_id_kind` exit 3 BEFORE anything persists (the rejected-
  write-persists-nothing rule from gate round 3 applies here verbatim).
- B4. Order of operations inside the command: validate target → store new
  memory → close old (`set_memory_valid_until(old, now)`) → journal
  `memory.meta_changed` payload `{superseded_by: new_id}`. If the close fails
  after the new memory committed, report `partial: supersession_pending` with
  the retry command — never silent.
- B5. Response: `superseded: {id, handle}` + `superseded_count` in metadata.
- B6. Contract: `REMEMBER_FLAGS` + `REMEMBER_DATA` entries in `registry.rs`,
  skill.md paragraph (the "correction" workflow), regenerate stamp.
- B7. Tests (`supersede_memory.rs`): old withheld from default recall with the
  expiry note; `--include-expired` reaches it; missing id exit 3 persists
  nothing; `chunk:` handle resolves; dedup + `--supersedes` compose (dedup hit
  supersedes the OTHER memory, not itself); double-supersede is idempotent.
- B8. Scoped Codex gate; fix round; ships as 0.4.2.

## Track C — document freshness: `documents status`, then `ingest --sync`
(~2–3 days + gate)

The field report's genuinely new capability. Split read-only-first so the
risky half ships after the safe half proves the diff logic.

- C1. Rulings first (`interfaces.md`, ~half a page): (a) document identity =
  absolute `source_path` as stored; a moved file is delete+add in v1, no
  rename detection; (b) staleness = sha256 of file bytes vs stored hash;
  (c) docs ingested without a stable path (stdin, URL-only) are
  `stale: "unknown"` and excluded from sync; (d) deleting a document's
  memories is the journalled soft delete — which composes with the 0.4.1
  retraction quarantine: derived artifacts stay withheld until recompile,
  automatically. State this composition explicitly; it is the part gate
  rounds exist to check.
- C2. Schema: additive `content_hash TEXT` + `synced_at TEXT` on `documents`
  via `add_column_if_missing` (documents is the evidence floor and survives
  the subtraction plan — additive is safe). Backfill: NULL = unknown.
- C3. Write path: `ingest` stores the hash on every new/re-ingested document.
- C4. Ship `documents status --json` alone: for each doc with a source_path,
  stat + hash → `unchanged | changed | missing | unknown`, with counts and
  exit 0 (it is a report, not a failure). `documents list` gains the same
  `stale` field per row. Zero write risk; ships immediately after review.
- C5. Then `ingest --sync <dir>`: changed → `documents delete` + re-ingest
  (compose the existing verbs inside one journalled sequence per file);
  missing → tombstone ONLY under `--prune`; unchanged → skip. `--dry-run`
  prints the plan without writing (same convention as directory ingest).
- C6. Report shape: `{re_ingested: [], pruned: [], unchanged: n,
  unknown: []}` — the caller must be able to see what a sync did without
  diffing the store.
- C7. Tests: edit file → sync → old chunks unfindable, new findable; delete
  file without `--prune` → reported, untouched; with `--prune` → tombstoned
  AND derived layer quarantined until compile (assert the 0.4.1 quarantine
  fires); stdin-ingested doc untouched + reported unknown; dry-run writes
  nothing (canary store hash before/after).
- C8. Scoped Codex gate; ships as 0.4.2 or 0.4.3 depending on B's timing.

## Track D — fact-extraction inversion rulings (design only, no code)

Highest-leverage change on the board (HANDOFF priority 1), NO-GO'd at gate 4
until these rulings exist. The deliverable is an `interfaces.md` v3 addendum,
reviewed by the same gate process — cheap to review, catastrophic to skip.

- D1. Authority ruling: `--facts-json` stops stamping `UserStated`; default
  becomes `agent_extracted` (no interval-closing authority on safety
  relations). Person authority requires an explicit separate act — define the
  act (`--facts-authority user` gated how?) or defer person-authority
  entirely to v0.5. Write the decision, not the options.
- D2. Fact-processing receipt ruling: the journalled, rebuild-safe state set
  `{complete_with_facts, complete_empty, text_only, deferred, partial,
  failed}` welded to the batch — including the empty batch. Kills the
  eligible-for-paid-re-extraction trap (gate-4 H2).
- D3. Bare-`remember` ruling: pick the write contract (explicit mode among
  facts-json | extract | text-only | defer; bare call = defer + loud
  `structured_state_ready: false`, or hard fail). One paragraph, one choice.
- D4. Edit-parity ruling: `edit --content` accepts replacement facts with the
  same modes; a correction must not strand structured state (gate-4 H3).
- D5. Offline ruling: `--offline` becomes a global network deny (embeddings,
  extraction, adjudication) or is renamed `--offline-embeddings`. Pick one.
- D6. Config ruling: fact-model settings move out of `compiler.*` before the
  subtraction deletes that section.
- D7. Shell-fallback (codex/claude as extractors): DEFER, recorded with
  gate-4 H4's sandbox criteria as the reopening condition.
- D8. Codex reviews the rulings document itself. GO on the design unblocks
  the implementation session.

## Track E — decisions that need the operator (not engineering time)

- E1. `people`/entities: keep a thin fact-backed `people` view and delete the
  compiler-derived entity graph (recommended), or delete wholesale? Blocks
  the subtraction plan's final shape (~989 relationship memories ride on it).
- E2. True erase/`purge` (PII/secret removal): two-tier design sketch exists
  (clean purge / redacted purge + journalled tombstone). Schedule it into
  v0.5 or explicitly park it.
- E3. Subtraction timing: arm it after A5 + D8, or hold longer.
- E4. The stale 0.4.0 on crates.io: leave unyanked (documented, coherent,
  never pointed at by the tap) — flip only if someone installs it by
  accident.

## Deferred with reasons on file (do not restart without them)

- Single-copy wire + compact agent-info: blocked on response v2 + trust
  marker (gate-4 H5/H7).
- Recall abstention floor: blocked on typed score provenance + calibration
  (gate-4 H8, M4); calibrate only after subtraction changes the source set.
- Research schema registration: surface is scheduled for deletion (accepted
  deferral, gate round 3).

## Hygiene tail (minutes each, batch into any session)

- H1. `cargo fmt` the two pre-existing `state_view.rs` diffs.
- H2. After A5: delete the archived old SKILL.md copies in the session
  scratchpad (auto-cleared on reboot anyway).
- H3. Consider bottling the tap formula (source build is minutes on install);
  low value until someone besides the operator installs it.

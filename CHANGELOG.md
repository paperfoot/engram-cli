# Changelog

## 0.4.0 — 2026-08-05

The five-move rebuild of the write/mutate/adjudicate layer, from the 2026-08-01
agent-experience audit: an append-only journal as source of truth, bi-temporal
interval facts, decision replay, a single-writer core with jobs, and a generated
contract. Schema v9 → v13 (v10 journal + intervals, v11 lessons, v12 decision
orientation, v13 event origin); the v10 migration writes a `VACUUM INTO` backup
beside the store before touching a row. v11 is additive-only. v12 adds one
column and backfills it, and takes no backup for a narrower reason: the only
column it writes is the one it just created, so no prior value is at risk. v13
stamps every journal event with its origin (`own` vs `imported`) so a rebuild
cannot launder an imported claim into trusted history.

### Breaking for agents/scripts

- `forget <unknown-id>` exits 3 `not_found`. It used to exit 0 with
  `{"deleted": false}`, so a typo was indistinguishable from a successful
  retraction. `deleted` keeps its meaning; `already_retracted` disambiguates
  the idempotent case.
- `forget` no longer erases the memory's facts. Like `edit`, it closes them
  as intervals (`close_reason: "retracted_source"`, authority `caller`) and
  reports `facts_closed`/`facts_closed_ids`. A script that asserted facts
  vanished after `deleted: true` will find them present-but-closed; `facts
  list` labels them `[closed: retracted_source]` and excludes them from
  active reads.
- Three more commands changed their wire error on an id that does not exist.
  `edit <uuid> --importance 3`, `facts resolve <uuid> --action keep-both` and
  `jobs show <uuid>` each answered `bad_input` in 0.3.1 and answer `not_found`
  now; all three still exit 3, so only a script branching on `error.code` sees
  the change — and that is precisely the script that should. An id that will
  never exist is not a malformed argument, and `bad_input` sent agents back to
  re-check their flags instead of their handles.
- `kb delete --confirm` remains a hard delete of the KB's projections (its
  journal events survive). It is the one deliberately destructive KB-level
  command and is documented as such rather than journalled — the append-only
  guarantees apply to memory- and fact-level operations.
- `edit --content` no longer deletes the memory's facts. They close as
  intervals (`close_reason: "corrected"`, authority `caller`) and the response
  says so: `facts_invalidated`, `facts_invalidated_ids`, `reextraction`.
  Adjudication history survives edits; `admin rebuild` can prove it.
- **Import never honours a close on a safety relation.** A `takes_medication`,
  `allergic_to` or `diagnosed_with` fact in an imported file (v1/v2/v3, rows or
  events) lands OPEN; the close is dropped whole and counted as
  `safety_closes_refused`. No field of the file decides this — authority,
  reason and interval shape are all written by whoever wrote the file, and
  three review passes each forged the field the previous fix consulted. The
  error direction is deliberate: over-reporting a prescription is visible and
  survivable, under-reporting one is not. A store's OWN journal is unaffected —
  events now record their origin (schema v13), so a rebuild cannot launder an
  imported claim into trusted history.
- `export` writes format version 3 (a journal slice — lossless, including
  retracted memories, decisions and the conflict queue). v2 and v1 files still
  import; a v2 import synthesizes genesis events.
- `suggested_action` on a SAFETY pair (`takes_medication`, `allergic_to`,
  `diagnosed_with`) is always `requires_context`, never `supersede-old`.
  `supersede-old` survives only where the classifier itself ordered the values
  and strict autonomy was the sole reason the pair queued. `allowed_actions`
  is unchanged — advice narrowed, permission did not.
- `arity` for the safety relations reads `"multi"`: a medication list is a
  SET. Two different drugs no longer queue a conflict at all; same-drug dose
  changes queue exactly one, safety-gated.
- `resolve_command` is one runnable, shell-quoted command with no pipe. The
  old `--action a|b|c` template executed its first branch when pasted. The
  structured `resolve {program, args, allowed_actions}` object sits beside it.
- Clap parse failures emit the JSON error envelope with exit 3 (`bad_input`).
  They used to print prose and exit 2, colliding with "config error".
- `fact_type` for new safety-relation facts is `assertion`, not
  `profile_slot`.

### Added

- Memory expiry, enforced at recall. `remember --valid-until <RFC3339|YYYY-MM-DD>`
  records when content stops being true (a bare date holds through the END of
  that UTC day, matching `recall --until`); once that instant passes, `recall`,
  `memory search`, `research` and the daemon endpoints withhold the memory from
  every retrieval path — raw chunks (dense and lexical) by exact memory, and
  derived candidates (claims, wiki pages, entities, relations) by KB
  confinement: a derived row is a rewrite/aggregate that may paraphrase an
  expired memory, so any KB holding an expired live memory has its derived
  candidates withheld too, the same shape as diary isolation. The withholding
  is never silent: `metadata.expiry` reports `expired_memories_excluded` and
  `excluded_derived_candidates`, and `--include-expired` (`include_expired`
  over HTTP; also on `research`) reaches everything again. An all-expired
  scope answers `absence.reason: all_candidates_expired`, never
  `no_candidate_matched` — expiry is a validity claim, not an absence claim.
  Re-remembering identical content with `--valid-until` expires the EXISTING
  copy (`valid_until_updated: true`) instead of silently dropping the flag.
  A `--valid-until` earlier than `--event-time` is rejected: the memory would
  expire before it happened. Expiry comparison normalizes timezone offsets
  (`datetime()`, not TEXT ordering), so imported non-UTC timestamps expire at
  the correct instant.
- `--json` output purity pinned by contract tests: exactly one JSON object on
  stdout on success; on failure stdout stays empty and stderr carries one JSON
  error object (when piped) whose `error.exit_code` matches the process exit
  code; tracing/diagnostics never reach stdout even under `RUST_LOG=debug`.
  Streaming surfaces are the documented exception: `jobs watch` emits NDJSON —
  one JSON object per line, one line per state change.
- Typed id handles (`mem:`/`chunk:`/`fact:`/`conflict:`/…) accepted by every
  mutator; bare UUIDs stay valid everywhere. A chunk id fed to `edit`/`forget`
  resolves to its memory (`resolved_from` says so); a wrong-kind id names the
  kind it actually was and the handle that would work. Every read result
  carries `memory_id`, `handle` and a provenance chain.
- The events journal: every projection write appends its event in the same
  transaction; `engram admin rebuild` drops and replays the fact and
  adjudication projections and reproduces byte-identical state.
- Decision replay: adjudications are keyed by semantic identity and replayed
  on re-derivation — the store never asks the same question twice
  (`decision.replayed` in the journal, `resolution: "replayed_*"` in
  responses). Each decision also records the value it retired
  (`fact_decisions.loser_object_norm`, schema v12), so the ruling travels with
  the value rather than with the slot: a re-derivation that names the pair in
  the other order applies the same outcome, and it keeps applying it after a
  `kb delete` has cascaded the conflict row away. The v12 migration backfills
  that orientation for every older decision whose conflict row is still there,
  so an upgraded store does not depend on a row a later delete can cascade
  away. Where no orientation was ever recorded and none can be recovered, the
  pair QUEUES rather than replaying by pair order — on every relation, not
  only the safety ones: a ruling applied backwards retires the value the
  caller kept, whether it names a prescription or an employer.
- Epistemic reads: `returned`/`total`/`cursor` on every listing, `verdict:
  results|no_match` with an absence certificate (scope searched, top score vs
  threshold, nearest subjects), `metadata.scope`, and honest `elapsed_ms`
  measured in dispatch on every command.
- `engram state <subject>` — the compiled current-state view: open intervals
  grouped by relation and member, provenance, open questions.
- Lessons (procedural memory): `engram lessons add|list|confirm|retract`,
  `engram reflect` distilling episodes and decisions into reusable lessons
  (schema v11, additive).
- Jobs: `engram jobs submit|list|show|cancel|resume|watch`, `compile --async`,
  sleep-time compile triggers, SSE progress over the daemon.
- A real HTTP server in `engram serve`: Content-Length framing, bounded
  bodies, per-connection tasks, keep-alive; `/v1/recall` carries the same
  `conflicts` array as the CLI.
- `ENGRAM_STORE` env var and `--store` flag: root db/vectors/config/cache
  under one directory for sandboxes and tests; `agent-info` answers without
  opening, creating, or migrating any store.
- `engram agent-info --schema '<command>'` prints one command's JSON Schema —
  input types, required arguments, and the documented output fields. Three
  error surfaces had been telling agents to run it since before the flag
  existed; it now exists, resolves aliases, and answers an unknown name by
  listing the commands that have a schema.
- `engram-contract`: the command schema as data, generating agent-info,
  skill.md and JSON Schema, with contract tests that fail the build when docs
  and binary drift (they caught two drifts during this build's own
  integration).

### Fixed

- `state` paging can no longer hide a relation: every relation open at as-of
  appears on every page with its true `open_count`; only `values` page.
  `values: []` beside a non-zero `open_count` means "on a later page", never
  "none" — previously `--limit` could return an answer with no
  `takes_medication` group at all while two medications were open.
- `state <subject> --relation R` on a resolved subject with no data for `R`
  answers `absence.reason: no_open_values` instead of `subject_not_found`
  (which it reported while simultaneously listing the subject as a
  near-miss at score 1.0). "No allergy data" and "no such person" are
  different answers.
- Cross-diary privacy: a completed non-compile job (e.g. a fact backfill) no
  longer counts as proof that a deleted memory has been compiled out of the
  derived layer. Only a completed `compile` un-gates a KB; the jobs schema is
  now ensured inside `SqliteStore::open` itself so every reader sees the
  columns the gate depends on.
- The live `entities` table gets its kind index back. The v2 migration renamed
  `entities` to `entities_v1_global` and SQLite carried `idx_entities_kind`
  with it, so the `CREATE INDEX IF NOT EXISTS` on the new table was a silent
  no-op — every store since, fresh or migrated, ran kind-scoped entity reads
  as table scans. `open()` now heals the index (check-then-repair, write-free
  once healed).

## 0.3.1 — 2026-07-28
- Failover hardening from the post-release code review (15 findings, all fixed):
  rotation now also fires on 400/401 key-invalid refusals (a deleted primary used to
  defeat the whole failover feature); the degradation note diagnoses a 429 as
  possibly-transient instead of telling you to replace a healthy key; rotation is a
  compare-and-swap from the observed key so concurrent refusals can never skip an
  untried fallback; `chunks repair` gains the failover chain via one shared
  `cloud_gemini` factory; `ingest` builds one embedder for the whole run and, like
  `facts backfill`, now surfaces `embedder_degraded`; the daemon omits the field
  when healthy instead of serializing null; `config check`'s `ok` is true only when
  a key in the chain actually serves, and probes run concurrently on one client.
- Date hygiene: unreadable stored fact dates fall back to the epoch (never "now");
  v2 import validates fact dates; the remember dedup path can no longer stamp
  "now" on facts attached to a vanished memory.
- "fact 4 of 3" messages are gone: a marker/commit failure says what it is.
- Doc surfaces (agent-info, --help, README) caught up with the 3.5/3.6 model defaults.
- Compiler defaults move a generation forward: extraction `google/gemini-3.5-flash-lite`,
  synthesis `google/gemini-3.6-flash` (newer than the old pro and cheaper on output; the
  synthesis completion budget doubles to 16384 because 3.6-flash spends thinking tokens
  from the same budget). Ad-hoc chat default becomes `anthropic/claude-opus-5`. The
  benchmark grader stays pinned on `gemini-3.1-pro-preview` so recorded runs stay comparable.
- Fallback Gemini keys: `keys.gemini_fallbacks` / `GEMINI_API_KEY_FALLBACKS`
  (comma-separated, priority order). A 429/403 rotates to the next key BEFORE any backoff;
  refused calls are never billed. `engram config check` live-probes every key in the chain
  (`ok | quota_or_credits_depleted | invalid_or_revoked`) — presence is not health.
- A key failover is never silent: `metadata.embedder_degraded` plus a stderr warning on
  recall, research, remember, edit, reindex and both daemon endpoints whenever a fallback
  key served, naming the masked key and the remedy.

## 0.3.0 — 2026-07-28

Two safety waves and a release gate. First publish since 0.1.4.

### Breaking

- Keyless cloud writes refuse: `remember`, `edit` and `reindex` exit 2 without a Gemini
  key unless `--offline` explicitly asks for the deterministic stub. Silently storing
  64-dim stub vectors made stores unsearchable under a later cloud key.
- `--facts-json` with a failing fact write is no longer reported as success.
- A blank daemon query is HTTP 400 before any provider or budget work.
- `answer_context` is the escaped `engram-result/1` envelope (see `agent-info`), replacing
  the old `[n] (kind, kb=…)` list. Recalled text is data, not instructions.
- Caller-supplied `--facts-json` memories stay on the `facts backfill` queue until real
  extraction has run once (the marker, not the presence of facts, retires a memory).

### Fixed

- `remember` is atomic: chunk + embed happen before any DB write; a failed embed can no
  longer commit a memory that recall cannot find. Same guard on empty `edit` rewrites and
  whitespace-only v1 imports.
- An unreadable extractor date falls back to the memory's own time, never "now" — a
  fabricated newest date used to win recency supersession against older true facts.
  Unreadable stored dates fall back to the epoch; v2 import rejects them.
- Compiled facts route through the canonical write path: `classify_pair`, slot policies,
  the conflict queue, and source-memory time. Compile can no longer blind-supersede a
  safety-slot fact (allergies, medication) or destroy multi-valued predicates.
- A fact batch and its extraction marker commit as one unit of work; a mid-batch failure
  strands nothing and a clean retry recovers everything.
- Fact-parse and provider errors never echo raw model output or memory text; non-2xx
  provider bodies contribute only their structured `error.message` field.
- `research` and both daemon endpoints hard-error on a cloud profile without a key and
  enforce spend budgets — parity with `recall` (they silently used the stub before).
- Paid embedding work is billed even when the call fails mid-way (multi-batch accounting);
  `edit` checks existence before embedding and attributes usage to the memory's kb/diary.
- Daemon parity: empty queries refused first, `layer`/`since`/`until` accepted on recall,
  provider metadata reported, and an omitted diary means the diary named `default` —
  exactly as the CLI — instead of every diary.
- `ENGRAM_BENCH_FORCE_STUB=0` no longer forces the stub (the value decides, not presence).
- The budget guard gates paid calls only: a zero spend cap no longer blocks free
  `--offline` writes.
- Strict-clippy baseline cleared workspace-wide (44 → 0 under `-D warnings`).

### Added

- `--offline` on `remember`/`edit`/`reindex` (API: `"offline": true`) as the explicit
  stub opt-in; `wave0_*.rs` / `wave05_*.rs` standalone test suites (86 new tests);
  `LICENSE` (MIT).

## 0.1.4 and earlier

Pre-changelog. See `git log`.

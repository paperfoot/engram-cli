# Changelog

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

# engram v0.4 → next session handoff

Written 2026-08-02; release-state section updated 2026-08-11. Everything below
is verified against the tree, not recalled.

## State right now (2026-08-11)

| | |
|---|---|
| Branch | `arch/v0.4`, pushed to `origin` (private repo `paperfoot/engram-cli`) |
| Working tree | clean |
| Tests | **1213 passed, 0 failed** |
| Schema on branch | **v13** |
| **Published** | **0.4.1 on crates.io (all 10 crates) + brew tap formula**, 2026-08-11, after a GO from six independent gate rounds. A **0.4.0** also exists on crates.io: published from `2373a91` (2026-08-05) by an earlier session that never updated this doc — it predates expiry/purity/index/harness work, is coherent, unyanked, and the tap never pointed at it. |
| **Installed binary** | **0.4.1** (cargo-installed, 2026-08-11, on explicit approval) |
| **Boris's live store** | **schema v13** — migrated 2026-08-11 after two rehearsals; counts intact (1557 memories / 4794 facts), quick_check ok, entity index healed. Backups: manual `…engram.pre-0.4.1-backup-202608110829` (full dir copy) + automatic `db.sqlite.pre-v10-backup.20260811073038` beside the store. Agent skill refreshed in all four skill directories (old copies archived in the session scratchpad). |

**Update this table when you publish or install. The stale "nothing published"
claim here is exactly what burned the 0.4.0 version number.**

## Rules that do not bend

1. **Never touch the live store** at `~/Library/Application Support/bio.199-biotechnologies.engram/`.
   Every test and manual run sets `ENGRAM_STORE` to a TempDir. A test that opens the real store
   is an automatic reject — this bug once migrated it and bricked the installed binary.
2. **Never install or migrate without explicit approval.** The v10–v13 migrations are one-way.
   The moment a v0.4 binary is on PATH, the next `engram` command anywhere migrates his store.
3. Never push `main`, never force-push, never skip hooks. Feature branches push freely.
4. Implementation lanes never grade their own work — an independent model with its own
   reproductions does. This caught three rounds of defects nothing else would have.

## What v0.4 did (done, merged, gated)

Rebuilt the write/mutate/adjudicate layer from the 2026-08-01 agent-experience audit:

- **Append-only journal** welded to every projection write; `admin rebuild` replays it.
- **Bi-temporal interval facts.** Safety relations (`takes_medication`, `allergic_to`,
  `diagnosed_with`) are SET-valued — the polypharmacy storm is dead (5 meds → 0 conflicts,
  proven at unit and CLI level). Interval-closing is gated to person-supplied authority.
- **Decision replay** keyed by semantic identity: an adjudicated question is never re-asked.
- **Single-writer Core**, real HTTP framing, jobs API.
- **Generated contract** (`engram-contract` → agent-info/skill.md/JSON Schema) with drift-failing
  tests; typed id handles; epistemic reads (totals, cursors, verdicts).
- `edit` corrects instead of erasing; `forget` closes intervals instead of deleting facts;
  export v3 is lossless; `ENGRAM_STORE` isolation exists.

### Review history — read this before trusting anything

Three independent gate passes, each finding what the previous fixes created:

| Pass | Result | Where the findings lived |
|---|---|---|
| Grok full-diff | 9 findings, 2 High fixed | `docs/arch/review-final-grok.md` |
| Codex gate 1 | **NO-GO**, 7 blockers | `docs/arch/review-final-codex-gate.md` |
| Codex gate 2 | **NO-GO**, 2 blockers (both lead integration errors) | `docs/arch/review-regate-codex.md` |
| Codex gate 3 | **NO-GO**, 3 blockers, all in the import trust boundary | `docs/arch/review-regate3-codex.md` |

**All gate-3 blockers are fixed and merged** (rulings G8–G10 in `docs/arch/interfaces.md`
v2.3 addendum), with Codex's own reproduction scripts refusing the attacks. **But a fourth gate
has NOT been run.** Do that before anyone considers installing.

The lesson from three passes: **every defect came from fixes composing**, not from single
changes. Re-gate after every fix round; never assume a fix is isolated.

## The new work — why, and in what order

Two things happened after the gates that change the plan.

**Four naive-agent usability runs.** Grok 4.5, Composer 2.5, GPT-5.6 Terra and GPT-5.6 Luna
were each given real memory tasks, a fresh store, and were **forbidden from reading any docs**.
Raw reports in `docs/arch/ux/`. All four independently hit the same two walls:

1. `remember` **silently stores no facts** without an OpenRouter key — it returns success while
   `facts_added: 0`, which makes `state`, `people` and `facts` dead ends. Composer marked this
   "success, 1 attempt" and never noticed; Grok noticed and burned two attempts recovering. A
   command that gives a false success to a trusting agent and a puzzle to a careful one is the
   worst possible shape.
2. `recall` **never says "I don't know."** Asked about a subject the store has never heard of, it
   returns unrelated memories as `verdict: results`. Terra: *"seriously wrong behavior for a
   question-answer memory tool."* Our own `bench personal` has reported `abstention_rate: 0.0`
   with negative controls at `top_score: 1.0` all along.

**A quantified subtraction analysis** (`docs/arch/subtraction-plan.md`): ~22,500 production
lines removable (~31%), 12 tables, six read verbs → two.

### Priority order (this supersedes the earlier v0.5 agenda)

> **2026-08-11 update:** a three-week external field report (0.3.1, heavy multi-agent
> use) independently reproduced the same failure classes and added one new capability
> (document sync). The execution sequence is now the **80/20 order in
> `field-report-deltas.md`**.
>
> **2026-08-11, later — Tier 1 SHIPPED as 0.4.1:** expiry-at-recall
> (`remember --valid-until` / `recall --include-expired`, two quarantines, SQL-side
> lexical cutoff, `all_candidates_expired`), the `--json` purity contract tests, the
> entity kind index heal, and the test-harness `ENGRAM_STORE` isolation fix. Six
> Codex gate rounds (five NO-GO, then GO) — reports in the session scratchpad,
> substance recorded in the 0.4.1 commit messages. Deliberately deferred on gate-4
> grounds: single-copy wire (H5 injection boundary) and the abstention floor (H8 no
> valid score) — they remain priorities 2–3 below, unstarted.

**1. Invert fact extraction.** Boris's insight and the highest-leverage change: engram pays
`gemini-3.5-flash-lite` via OpenRouter to re-read text a *frontier model just wrote*. The caller
is the LLM. Make caller-supplied facts the default path (`--facts-json` already exists with full
contradiction detection and zero cost); call an LLM only when explicitly asked and configured.
Optionally shell out to `codex exec` / `claude -p` as a fallback for the agentless case
(`engram ingest` over PDFs). Wins: safer (no silent half-store), leaner, faster (no round-trip),
lighter (no key), better (frontier > flash-lite), and it makes `--offline` genuinely coherent.

**2. Single-copy wire + compact `agent-info`.** Pure deletion, no semantics change.
Measured: learning the tool **37,422 → 2,315 tokens** (93.8% less) by making `agent-info` a
capability index and letting agents pull one schema on demand via the `--schema` flag already
built. A 10-hit recall **5,774 → 1,192 tokens** (79.4% less) by sending each result once —
`answer_context` duplicates `results[].content` with different escaping, so deleting one also
kills a class of dual-representation bugs.

**3. Recall abstention.** The only item here that *adds* code, and it's small: a relevance
floor so a weak best-match returns `no_match`; rename the "absence certificate" to a search
receipt (it certifies scope traversal, not absence); `ready_for_synthesis` must require relevant
evidence, not merely any citation. A genuinely provable "no" is possible for facts —
"no open `blood_type` fact for subject X" — which the interval model makes available and nothing
uses.

**4. Structural cuts, after 0.4.0 is proven on his store.** Delete the
compile→claims→wiki→entities→research subsystem (8,935 lines, 8 tables; also removes the
polarity-inversion grounding bug and N+1 LLM calls inside a transaction); delete
`lessons`/`reflect` (1,877 lines — `lessons add` assumes its caller is human and auto-marks
entries confirmed at 0.9 confidence, which is false for an agent-driven tool); drop 4 dead
tables and 31 unused columns (this also uncovered a live bug: a migration renamed `entities`
and carried its index with it, leaving the live table **unindexed**).

### Two bugs in shipped v0.4 code, small and worth fixing early

- `state <subject> --limit N` **silently empties the answer**: with 2 medications and
  `--limit 1` it returns `total: 2, returned: 1, groups: []`. Paging runs over facts before
  they are grouped by relation.
- `state <subject> --relation R` returns `subject_not_found` when the subject *was* found but
  has no data for that relation — while listing that subject in `nearest_subjects` at score 1.0.
  "No allergy data" and "no such person" must not be the same answer.

## Decisions only Boris can make

1. **Does `people`/`entities` survive?** The subtraction plan deletes the networking view. His
   live store has ~989 memories in a `relationships` KB. Suggested middle path: keep `people` as
   a thin read over facts, delete the compiler-derived entity graph beneath it.
2. **When to install and migrate**, and whether to rehearse on a copy of the real store first
   (recommended: yes, and diff the result).
3. **Whether 0.4.0 ships publicly at all**, or runs locally for a week first. Breaking changes
   for agents are real and listed in `CHANGELOG.md`.
4. Whether to add a true **erase** command. `forget` retracts; content survives in `memories`,
   in chunks, and in the journal's `memory.written` payload. There is no way to remove a secret
   or PII. Design sketched in conversation: a two-tier `engram purge` (clean purge when nothing
   has derived from it; redacted purge afterwards), and the tombstone must itself be an event or
   `admin rebuild` resurrects the content.

## Map

| Path | What |
|---|---|
| `docs/arch/HANDOFF.md` | this file |
| `docs/arch/interfaces.md` | the binding contract — rulings R1–R15, D1–D28, G1–G10 |
| `docs/arch/redesign-brief.md` | lane ownership + hard constraints |
| `docs/arch/subtraction-plan.md` | the quantified removal plan |
| `docs/arch/ux/` | four naive-agent usability runs |
| `docs/arch/agent-era-findings.md` | what Anthropic's current guidance implies for engram |
| `docs/arch/v05-agenda.md` | the four deep design reviews, synthesized |
| `docs/arch/field-report-deltas.md` | 2026-08-11 field-report dispositions + the 80/20 execution order |
| `docs/arch/next-cycle-plan.md` | step-level plan for the post-0.4.1 cycle (phases 0–6) |
| `docs/arch/review-nextcycle-ultra.md` | ultra plan review: 5 High findings, revised phase order |
| `docs/arch/review-*.md` | every gate and review report |
| `CHANGELOG.md` | 0.4.0 breaking changes, agent-facing |

Codex reproduction scripts live under
`/private/tmp/claude-501/.../scratchpad/regate*/` — they are the acceptance criteria for the
trust-boundary fixes and will be lost on reboot. Re-create from the reports if needed.

## How to start the next session

```
Read docs/arch/HANDOFF.md. We're on arch/v0.4. Start with priority 1
(invert fact extraction) — do NOT install or migrate my live store.
```

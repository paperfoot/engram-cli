# TypeSafe JEV as a calibrated judge in engram — report

Branch `exp/jev-judge`, 2026-09-25 to 2026-09-27. Experiment branch, not a
release. Nothing merged to `arch/v0.4`, nothing installed, no migration.

## What JEV is, as used here

A hosted "decision" model: one `state` plus typed questions (noul = P(yes),
choice, score) in, calibrated probabilities out, no text. Pinned `jev-1.13.0`
on the direct API (`keys.typesafe` / `TYPESAFE_API_KEY`), with the same API
through OpenRouter (`typesafe/jev-1.13`) as the fallback route. Measured on
the direct route with a pooled client: p50 ~250 ms, p95 ~650 ms per batched
request; $0.042 per million input tokens. Every eval in this report together
cost under $0.05 of judge time.

## Results per site

Every number below is reproducible from `benchmarks/jev/*` (data under the
gitignored `data/scifact/` and `data/pairs/`). Judge numbers run through
`crates/engram-bench/examples/judge_eval.rs`, which builds requests from the
shipped questions module, so the evaluated wording is the wording engram sends.

### 1. Grounding: is an extracted claim/fact supported by its source text?

| Eval set | Labels | Lexical rule (today) | JEV |
|---|---|---|---|
| SciFact, 1,189 claim/abstract pairs | human | P 0.587 / R 0.756; admits **69%** of contradicting claims | AUC 0.965; p≥0.5: P 0.944 / R 0.827, admits 2.3% |
| compile's own extraction, 100 abstracts (313 supported + 535 adversarial rewrites + 35 unsupported) | Opus 5.5, 50 spot-checked | AUC 0.50; admits **99%** of rewrites | AUC 0.99; p≥0.9 keeps 100% of claims / 98.9% of facts, admits 1.3% of rewrites |
| remember's own extraction, 161 facts (60 LoCoMo windows + 20 health notes) | Opus 5.5 | no check at all; 16% of facts unsupported (22% on health notes) | AUC 0.79 (state-aware rendering); safety relations only at p≥0.5: 0 true facts dropped, fabricated allergy/medication blocked |

Rewrites JEV still admits at p≥0.9: association→causation 12%, hedge
removed 14%, prior-study attribution 10%. Direction flips, negations and
species swaps: 0–1%.

Spot-check of the Opus labels (50 items, read by Claude in the main session,
not a human): 42/50 agreement; all 8 disagreements are Opus being stricter than
the passage (a dropped "approximately", a missing population qualifier). The
supported and perturbation labels are reliable; the extracted-unsupported
stratum is noisy. `benchmarks/jev-grounding-spotcheck-20260925.json`.

Live runs with the real judge on the branch:
- compile: "Ana allergic_to penicillin" from "not allergic" — admitted by the
  lexical rule, p 0.01, not stored with the judge on.
- remember: "Sam allergic_to ibuprofen" from "avoid ibuprofen" p 0.02 and
  "takes paracetamol" from "paracetamol is fine" p 0.37 dropped;
  "allergic_to penicillin" p 0.99 kept.

### 2. Citation check on compile synthesis — not built

`llm/overview.md` is one wiki blob; its citations are checked for existence
only, and compile is scheduled for deletion (subtraction plan §2). The bench
QA suites never read compile output, so LitQA2 / ALCE before-after would show
nothing. Measured nothing, built nothing.

### 3. Conflict adjudicator — measured, not wired

148 labelled pairs (100 mined MemoryAgentBench updates + 48 handwritten
safety/restatement/look-alike/correction pairs). No existing bench exercises
the adjudicator: MAB conflict resolution is retrieval + answering.

| | today's LLM adjudicator (verbatim) | JEV signals + a code rule |
|---|---|---|
| engram-semantics pairs (48) | 43/48 | **48/48** |
| mined MAB single-value updates (100) | 87/100 | 76/100 — every miss is a second citizenship/language/employer, which engram's registry treats as set-valued anyway |
| signal AUCs | — | restates 1.00, same_slot 1.00, contradicts 0.97, old_ended 0.99 |
| latency p50 | 1.9 s | 0.24 s |

The adjudicator runs only under `--autonomy full`. Its real defects were
fail-open bugs, fixed on this branch whatever judge is used: a failed verdict
cleared the conflict queue (safety questions included) and reported success;
`reject_new` could discard a caller's dose change; the call held the SQLite
write lock across a network call.

### 4. Novelty-gate uncertain band — not wired

The band only queues `paraphrase_slot` pairs (same subject, single-valued,
cos 0.88–0.95): low volume. The root-cause defect was elsewhere and is fixed:
a gate "duplicate" absorbed a fact about a different subject (Carol's
metformin into Bob's). The gate now compares a fact only with its own
subject's facts and never absorbs across a safety relation.

### 5. Reranking — not run

No regression target: Cohere was not re-measured. JEV as a reranker is the
most promising route to the deferred abstention floor (it yields a calibrated
per-passage score), but the floor is Phase 6 and response-v2 work.

## Recommendation per flag

| Flag | Recommendation | Why |
|---|---|---|
| `judge.grounding` | **on** for your store (owner decision); shadow first for a week if you want your own numbers via `ENGRAM_JUDGE_LOG` | Blocks fabricated safety facts on both write paths; drops nothing true in the evals; fails soft |
| `judge.adjudicate` | **off** (not wired) | Low-volume site; the fail-closed fixes matter more than the judge. Research argues for deleting `--autonomy full` instead |
| `judge.dedupe` | **off** (not wired) | Low volume after the gate fix |
| rerank | not built | Needs a Cohere-vs-JEV run on conversational recall first |

## Other changes on the branch (not JEV)

- Security: the Gemini key rode in request URLs and leaked through error
  text; now a header. A real Gemini key was hard-coded in an engram-embed test
  and shipped in crates 0.3.1–0.4.1 (public on crates.io): replaced with a
  fake; the key must be rotated. TOML parse errors no longer quote config lines.
- Provider status: `config check` live-probes every key and reports role,
  health, credit left (OpenRouter's account and key balance; others do not
  expose one) and one fix; `doctor` shows the same offline. README gains a
  requirements and providers table.
- Agent tokens: a typical session drops from 53,407 to 15,580 tokens (−71%):
  `agent-info --command`/`--index`, a 719-token signpost skill, every command
  in the contract.
- `engram bench` no longer opens (and migrates) the live store.
- Keyless `ingest`/`chunks repair` refuse instead of writing unusable stub
  vectors; recall's dense scan reads vectors only (−11–15% peak memory).
- The close event journals which rule or judge closed a fact; `admin rebuild`
  keeps it (the one journal change this branch allows).
- Compile makes every provider call before its write transaction opens.

Tests: 1,213 → 1,391 passing, 0 failing.

## Open for the owner

1. Merge `exp/jev-judge` into `arch/v0.4`?
2. `judge.grounding` default on, or opt-in?
3. Rotate the leaked Gemini fallback key.
4. The relation registry has no aliases for `is_allergic_to` etc.; an
   extracted fact with an unregistered safety predicate skips the safety gate.
5. `facts backfill` still writes stub vectors silently when keyless.
6. Whether this goes into the next release.

# Field-report deltas — 2026-08-11

Source: a three-week field report from a heavy external deployment — a multi-agent
workspace with ~70 curated memories, a ~50-file markdown corpus ingested as documents,
3+ concurrent agent sessions writing, and a strict newest-date-wins evidence discipline —
running the **installed 0.3.1 binary**. Every item traces to a live incident. The raw
report stays local by design; this doc records only the engineering deltas.

## Why it matters

Independent convergence. With no access to these architecture docs, the report reproduced
the same failure classes found by the four design reviews (`v05-agenda.md`) and the four
naive-agent UX runs (`ux/`):

- stale outranking fresh (Theme 2's epistemic-retrieval gap),
- enrichment failing silently behind a success exit (the wall all four naive agents hit),
- token-bloated recall output (the measured `answer_context` duplication),
- retrieval backends degrading silently.

Three unrelated evaluation methods landing on the same defects is validation of the
v0.4 → v0.5 priority order, not a reshuffle. The report also independently praises what
the subtraction plan keeps: hybrid retrieval quality, the injection quarantine,
`event_time`, and the documents layer.

## Dispositions

| Field proposal | Disposition |
|---|---|
| Supersession + temporal ranking | Fact level already built in v0.4; recall-ranking half is Theme 2. Two cheap adds below. |
| Document sync as a verb | **Genuinely new — adopted into v0.5** (Theme 5). |
| Async enrichment job queue | **Declined — dissolved.** Extraction inversion removes the metered dependency; the subtraction plan removes the jobs subsystem. |
| One id namespace, strict `--json` | Mostly fixed on branch (recall carries `memory_id` beside chunk id). One cheap add below. |
| Compact, tiered recall output | Is HANDOFF priority 2, already measured. Tiering corrected below. |
| Importance decay unless pinned | Folded into Theme 2 with guardrails below. |
| Pointer memories (dedupe vs docs) | Deferred; depends on document sync existing. |
| Graceful retrieval degradation | Partially Theme 2 (silent backend failure) + extraction inversion; per-leg fallback lands with that work. |

### Adopted cheap adds

1. **Expiry at recall.** Honor `valid_until` in default recall; `--include-expired` to
   override. A memory with a natural expiry date is wrong the day after and nothing
   demotes it today.
2. **`remember --supersedes <id>`.** Memory-level demote-and-link, keeping history. Users
   currently maintain a manual edit-the-stale-memory discipline across every agent — the
   system property is missing. The write-time contradiction detector is the natural
   trigger: on conflict, propose supersession instead of storing co-equal truths.
3. **`--json` as a hard contract.** Exactly one JSON object on stdout, everything else to
   stderr, exit codes per documented failure class — enforced by a contract test, not
   convention.

### Adopted new capability: document sync

`ingest --sync <dir>`: hash/mtime diff against stored docs, re-ingest changed, tombstone
deleted, report untouched; store the source hash on every chunk; `documents list` shows
`stale=true`. Observed incident: an ingested doc froze at its ingest-time text and kept
serving content deliberately deleted from the source file — 0.87 similarity to the live
file, silently. In a medical corpus that failure is a hazard, not a nuisance. Fits the
report's best design principle: **files are canon; engram is the index and the event log.**

### Guardrails on ranking decay (Theme 2)

- Decay applies to **ranking, never confidence**. The reviews already caught the
  metformin-since-2010 category error (long-true fact punished 0.95 → 0.85); ranking
  decay bolted on carelessly repeats it one layer up.
- `access_count` as a ranking input is a rich-get-richer feedback loop; use with care or
  not at all.
- `--pin` for true invariants (identity, hard constraints) is a good primitive.

### Correction to the report

It asks for compiled claims to rank as the top provenance tier. The design reviews found
claim grounding is ~50% lexical overlap and accepted a polarity-inverted claim, which is
why compile→claims→wiki is scheduled for deletion. The tiering instinct is right; the
trustworthy hierarchy is **facts > curated memories > doc chunks**, with claims gone.

## 80/20 execution order

Tier 1 is the 20% that buys most of the value: mostly deletions and small fixes, each
hours-to-a-day, each with a measured or reproduced payoff. Order within tiers is priority.

**Tier 1 — cheap, disproportionate payoff**

1. The two shipped `state` bugs: `--limit` silently emptying grouped answers, and
   "no data for relation" conflated with "no such subject". Hours each; the exact
   failure class the rebuild targeted.
2. Single-copy wire + compact `agent-info` (HANDOFF priority 2). Pure deletion.
   Measured: recall 5,774 → 1,192 tokens; agent-info 37,422 → 2,315.
3. Recall abstention floor + rename absence certificate → search receipt
   (HANDOFF priority 3). Small add; kills "recall never says I don't know".
4. `--json` stdout purity + exit codes as contract tests.
5. Expiry at recall (`valid_until` honored by default).

**Tier 2 — the big leverage**

6. Invert fact extraction (HANDOFF priority 1 — the highest single-leverage change;
   sequenced after Tier 1 only because those are hours, this is days).
7. `remember --supersedes <id>` + contradiction-triggered supersession proposal.
8. `ingest --sync` document freshness.

**Tier 3 — after 0.4.0 is proven on a live store**

9. Structural cuts per `subtraction-plan.md`.
10. Theme 2 epistemic ranking: temporal clustering of near-duplicates, ranking decay
    with `--pin`, provenance tiers in recall output.

Gate 4 (independent re-review) still runs before anything is installed or migrated,
regardless of tier.

# What four independent reviews say engram still is not

Four reviewers, disjoint angles, no shared context: reasoning layer, retrieval quality, data
model, agent economics. Raw reports in `review-angle-*.md`; Anthropic's current guidance in
`agent-era-findings.md`. This is the synthesis.

They converge hard, from four directions, on one sentence:

> **engram stores and retrieves TEXT and ROWS. It does not represent BELIEF.**

The v0.4 rebuild made the write path trustworthy — nothing is destroyed, decisions survive,
safety intervals cannot be closed by a machine. That was the right work and it holds. But the
question an agent actually asks is *"is it true, how well do we know, and what would change my
mind"*, and engram answers *"here are some rows and some text."*

---

## Theme 1 — no stance, no modality, no coverage

**`state` answers row-existence, not the agent's proposition.** `state Ana --relation
allergic_to` returns `no_match / subject_not_found` while listing `nearest_subjects: ["Ana"]` —
the subject it matched exactly. Five different truths collapse into one shape: unknown person,
known person with no allergy data, recorded as *not* allergic, allergic, contested. For "can I
prescribe penicillin?" those must be different answers.

**A `Fact` row collapses proposition, assertion, and belief.** One `source_memory_id`, one
scalar confidence, one coarse `SourceKind`. "Alice says Dr Lee suspects Ana may have MS" is
stored indistinguishably from a diagnosis; the extractor simply drops hedges. There is no
polarity or modality — no `denied`, `suspected`, `ruled_out`. `confirmations` counts repeats,
so copied sources inflate belief that no independent evidence supports.

**Provenance proves addressability, not support.** `state` cites the *first* chunk of a fact's
source memory; the reviewer reproduced a fact extracted from the last chunk displaying a
snippet of unrelated project-planning text. Claim grounding is ~50% lexical overlap, not
entailment: a source saying rapamycin *"did not decrease mortality"* accepted a claim that it
*"decreased mortality."* A citation that points at text contradicting the claim is worse than
no citation.

**Confidence is a category error.** It adds extraction fidelity + source class + confirmations
+ age + contest status and calls the sum calibrated. Its staleness term uses `valid_from` —
when the state *began* — not when evidence was last seen. A user confirming today that they
have taken metformin since 2010 gets confidence *reduced* 0.95 → 0.85 for the crime of the fact
being long-true.

**The fix, and it is one fix:** separate immutable propositions from many assertions, each
carrying source span, asserter, extractor version, polarity, modality, directness and authority;
make `facts` a rebuildable belief projection over them. Then add the missing verb:

```
engram assess <subject> --relation R [--object O]
  stance:   supported | refuted | contested | unknown
  coverage: complete | partial | unassessed
  evidence_for / evidence_against / blocking_questions / last_verified
```

Replace the composite scalar with an epistemic vector. Publish the basis; stop publishing a
number that invites comparison it cannot support.

## Theme 2 — retrieval cannot say "I don't know"

There is **no relevance cutoff**. Recall admits the top-ranked chunks whatever they score, so a
semantic `no_match` is effectively impossible while the scope holds any chunk. Reproduced: a
store containing only *"Alice prefers green tea"*, asked *"What is Bob's blood type?"*, returns
the green tea line with `verdict: results` — and in `research`, `ready_for_synthesis: true`,
because that flag is just `cited_results > 0`.

**Our own benchmark has been reporting this all along**: `abstention_rate: 0.0`,
`negative_controls: 6`, `negative_mean_top_score: 1.0`. Six queries built to have no answer,
all answered, all at perfect confidence. It was quoted approvingly in this build as evidence
the bench was healthy.

So the "absence certificate" certifies **scope traversal**, not absence. It should be renamed
`search_receipt`; `no_match` should be reserved for a calibrated null-relevance decision; and a
*genuine* absence certificate is only possible for closed-world reads over a defined projection
("no open `blood_type` fact for resolved subject X") — which the interval model now makes
possible and nothing uses.

**Recall ranks immutable text, not epistemic state.** Facts, intervals and decisions are not
retrieval candidates at all. A returned chunk carries no validity interval and no
current/historic/contested marker, so an adjudicated 500 mg → 1000 mg change still returns the
500 mg chunk at full score. The closed-fact sideband scan looks at 20 facts, matches subjects
and objects but not predicates, is not attached to the result it qualifies, and never appears
in `answer_context`.

Backend failures are also silent: lexical, claim and wiki errors become empty runs with a
tracing warning while the response still presents itself as hybrid retrieval.

## Theme 3 — the agent pays 10–50× for what it needs

Measured on 0.4.0, exact bytes:

| Surface | Scenario | Tokens |
|---|---|---:|
| `agent-info` | read once per session | **37,420** |
| `SKILL.md` | always loaded | 5,738 |
| `recall` | 10 hits | 5,531 |
| — of which actual content, once | | **542** |
| `state` | 50 facts | 24,748 |
| `state` | default ceiling, 200 | **96,046** |

`recall` returns `answer_context` *and* `results[].content` — the same text twice — plus UUIDs,
provenance, and seven metadata fields duplicated between `data.metadata` and the envelope. The
advertised `token_budget` measures only retrieved content: it reports 760/800 used while
emitting 5,396 tokens of JSON. `state` and `facts list` have no output budget at all.

Anthropic's guidance is the yardstick: *"the smallest possible set of high-signal tokens"*, and
a distilled sub-agent return of **1,000–2,000 tokens**. We are an order of magnitude over on
every surface an agent touches every turn.

Fixes, cheap and immediate: budget the *serialized* response, not the retrieved text; a
`--format concise` that drops `answer_context` and returns expandable handles; make
`agent-info` a small core with per-command detail via the `--schema` flag this rebuild already
shipped; and shrink `SKILL.md`, which is written to defend against mistakes a Claude 5 model no
longer makes.

## Theme 4 — identity and time are under-modelled

**Facts reference strings, not entities.** There is no `subject_entity_id`. An alias is unique
per KB, so an ambiguous "Maria" must be assigned to exactly one person or invent a third. Merge
deletes the losing entity and does not rewrite facts, so Jane Smith and Jane Doe never become
one history.

**`member_identity` is doing ontology with token deletion.** Its real failure distribution:
`Glucophage XR` vs `metformin extended-release` under-merges; `Augmentin` vs
`amoxicillin/clavulanate` cannot be expressed; `thyroid cancer (papillary)` and `(follicular)`
over-merge once parentheses are stripped; `BRCA1: c.68_69delAG` is truncated at the colon;
non-English dose language survives as identity. A cessation stated by brand may not match the
open generic interval — which is a safety failure, not a nuisance.

**Time cannot be uncertain.** No "approximately March" (coerced to March 1), no recurrence, no
distinction between "known ongoing", "end not stated" and "end unknown". And the flaw shipped
*this round*: a cessation with unknown start is written `[end, end)` — which our own algebra
defines as empty and overlapping nothing. It should be `(-∞?, end]`.

**The two temporal axes are conflated.** The generated contract claims `--as-of` answers "what
did we believe in March"; it only filters valid-time and never applies a transaction-time or
journal cutoff, and provenance labels `event_time.unwrap_or(created_at)` as `observed_at`,
collapsing the axes in the response. The contract system built to stop docs lying is carrying a
lie it cannot detect, because the claim is semantic rather than structural.

**The registry is configuration masquerading as ontology.** Unknown predicates silently become
set-valued/mutable/text, facts bind predicate *text* rather than the registry *version* that
interpreted them — so adding an entry later retroactively changes the meaning of years of
stored facts with no reinterpretation event.

## Theme 5 — no lifecycle, and no multi-machine story

Append-only forever with no compaction, no forgetting curve in practice, no cost ceiling.
Diaries are namespaces, not collaboration: no changefeed, no acknowledgment cursor, no "what
changed since my last turn". For two machines there is no convergence protocol at all — events
carry local `seq` with no store/replica identity or causal parent, and export is a full
snapshot with no `since_seq`. Tested: two stores edited the same memory, cross-imported, and
ended holding both revisions in opposite order with different materialised content and no
remaining convergence step. Handles carry no store or revision identity, so the same
`mem:<uuid>` can name divergent content on two replicas with no way to detect staleness.

Near-term honest answer: one canonical daemon per user, other machines as clients. Say so
explicitly rather than letting export/import look like sync.

---

## What this means for 0.4.0

Ship it. It fixes real, reproduced data destruction and it is a large improvement on 0.3.1.
But describe it accurately: **the write path is now trustworthy.** Not "engram reasons."

Two items are cheap and should land before it goes on a live store:

1. **`state --limit` silently empties the answer.** With two medications and `--limit 1`:
   `total: 2, returned: 1, groups: []`. Pagination runs over facts before they are grouped by
   relation, so any limit below the fact count can return an empty medication list while the
   envelope claims results. This is the failure class the whole rebuild targeted, on the
   surface no gate was pointed at.
2. **Rename the absence certificate.** Calling scope traversal an absence certificate is
   precisely the dishonesty this rebuild set out to remove, and renaming costs nothing.

Everything else is v0.5, in this order: assess + proposition/assertion split (Theme 1) → honest
abstention and epistemic retrieval candidates (Theme 2) → response budgets and progressive
disclosure (Theme 3) → typed values, entity-referencing facts, uncertain time (Theme 4) →
lifecycle and sync (Theme 5).

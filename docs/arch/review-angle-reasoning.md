Verdict: engram v0.4 is now a strong memory substrate, but its “reasoning layer” can still make an agent more confidently wrong. The central mistake is treating stored assertions and agent actions as if they already carried human authority and sufficient evidence.

I reviewed the reasoning code at `4f1ad71`; during review, HEAD advanced to `dc3c758` through a docs-only commit, so the cited implementation did not change. I intentionally excluded the previously audited gate areas.

## Ranked findings

1. **Procedural memory is a self-authorizing feedback loop, not learned policy.**

The code assumes whoever invokes the CLI is human: `lessons add` immediately writes `status=confirmed`, `source_kind=user_stated`, confidence `0.9`, and a recallable twin because “a human typed the sentence.” But agent-info explicitly tells AI agents to invoke that command. [lessons.rs:300](/Users/dev/Projects/engram-cli/crates/engram-cli/src/commands/lessons.rs:300), [agent_info.rs:270](/Users/dev/Projects/engram-cli/crates/engram-cli/src/agent_info.rs:270)

That creates recursive epistemic amplification:

- An agent makes a bad inference.
- The same agent records it as a confirmed, user-stated lesson.
- Future agents retrieve it as durable guidance.
- An agent can also “confirm” its own `reflect` proposal; `caller` is transport provenance, not human authority.
- Retraction changes only the lesson row. The recallable memory twin remains live. [lessons.rs:499](/Users/dev/Projects/engram-cli/crates/engram-cli/src/commands/lessons.rs:499), [lessons.rs:549](/Users/dev/Projects/engram-cli/crates/engram-cli/src/commands/lessons.rs:549)

I reproduced the last failure: after retiring “Always deploy with the retired blue protocol,” `lessons list` showed it as retired, while ordinary `recall` still returned the old procedural memory.

The deterministic proposers compound this. A quick edit becomes “re-read before storing”; three closes become “churn”; any retracted fact-producing memory becomes a “bad source.” Those are causal interpretations of telemetry without goals, outcomes, reasons, counterexamples, or exceptions. [reflect.rs:177](/Users/dev/Projects/engram-cli/crates/engram-cli/src/commands/reflect.rs:177), [reflect.rs:217](/Users/dev/Projects/engram-cli/crates/engram-cli/src/commands/reflect.rs:217), [reflect.rs:266](/Users/dev/Projects/engram-cli/crates/engram-cli/src/commands/reflect.rs:266) Moving this synchronous code into a background job would only automate the same mistake.

I would introduce explicit `actor_kind` and authority: human, agent, service, imported. Agent-written lessons remain proposed unless human-attested or promoted by a declared evaluation policy. A real lesson should have typed conditions, action/avoidance, expected outcome, scope, exceptions, supporting episodes, counterexamples, and supersession. Deterministic rules should produce anomaly candidates, not lessons. Retraction must atomically deactivate the twin, and action-time reasoning needs an applicability matcher rather than hoping semantic recall finds a slug embedded in prose.

2. **`state` compiles assertions, but gives no answerability or knowledge-coverage signal.**

The contract tells agents that `state` returns what is “TRUE NOW.” [agent_info.rs:66](/Users/dev/Projects/engram-cli/crates/engram-cli/src/agent_info.rs:66) In reality, an open interval means only “an assertion has not been closed.” It does not mean verified, complete, uncontested, or safe to act upon.

The empty case is worse. Whenever the resulting groups are empty, the code emits `subject_not_found`, regardless of why. [state.rs:545](/Users/dev/Projects/engram-cli/crates/engram-cli/src/commands/state.rs:545), [state.rs:751](/Users/dev/Projects/engram-cli/crates/engram-cli/src/commands/state.rs:751) In an isolated probe, Ada had an exact `lives_in` fact; `state Ada --relation takes_medication` returned:

```text
resolved_from: exact
verdict: no_match
absence.reason: subject_not_found
nearest_subjects: Ada, score 1.0
```

For `allergic_to`, an empty result could mean never asked, explicitly “no known allergies,” all prior allergies closed, no observation in the requested period, or simply no extracted facts. The current “absence certificate” certifies that a query ran, not what may be inferred from its emptiness.

The missing primitive is a question-scoped belief object. Every requested relation should return an epistemic status such as `supported`, `contested`, `unknown`, `known_empty`, `stale`, or `insufficient_coverage`, plus `last_verified_at`, coverage provenance, blocking questions, and supporting/refuting evidence. I would add an `assess` surface that combines those fields with applicable confirmed lessons. Keep `state` as a raw current-belief view, but stop calling unclosed assertions truth.

3. **The bi-temporal model is not exposed as bi-temporal reasoning.**

`--as-of` filters only the fact’s valid interval. [state.rs:562](/Users/dev/Projects/engram-cli/crates/engram-cli/src/commands/state.rs:562) It cannot answer “what did the agent know then?” Worse, provenance labels the memory’s backdated `event_time` as `observed_at`, choosing it over the actual creation time. [state.rs:430](/Users/dev/Projects/engram-cli/crates/engram-cli/src/commands/state.rs:430), [sqlite.rs:3097](/Users/dev/Projects/engram-cli/crates/engram-storage/src/sqlite.rs:3097)

I backfilled a fact on 2 August 2026 with event time 1 March 2024. `state --as-of 2024-04-01` returned the fact and reported `observed_at=2024-03-01`; the journal transaction timestamp was 2 August 2026. A historical agent replay therefore receives knowledge that did not exist yet. It also receives today’s open-question queue because that query is not cut off by `as_of`.

Expose both axes:

- `valid_at`: when the assertion says the world state held.
- `known_at`: the journal cutoff for what engram had observed or decided.
- `observed_at`: actual transaction/observation time.
- `reported_event_time`: the source’s claimed effective time.

Then add `state changes`: distinguish “the world state changed,” “we learned something new about the past,” “a belief was corrected,” and “a question was resolved.” The append-only journal and intervals already contain most of what this needs.

4. **Provenance currently proves addressability, not evidential support.**

For a fact, `state` loads every chunk belonging to its source memory and cites the first one. [state.rs:430](/Users/dev/Projects/engram-cli/crates/engram-cli/src/commands/state.rs:430) The compiler knows the exact chunk it is processing, but sends only `memory_id` into the fact write path. [compile.rs:934](/Users/dev/Projects/engram-cli/crates/engram-cli/src/commands/compile.rs:934)

I reproduced this with a multi-chunk memory: the fact was extracted from the final chunk containing an allergy statement, while `state` displayed a first-chunk snippet consisting entirely of unrelated project-planning text.

The claims/wiki path has the same conceptual weakness:

- Claim “grounding” is numeric matching plus 50% lexical overlap, not entailment or polarity. [compile.rs:1193](/Users/dev/Projects/engram-cli/crates/engram-cli/src/commands/compile.rs:1193)
- My source said rapamycin “did not decrease mortality”; the stub proposed “decreased mortality.” It was accepted: one claim written, zero rejected.
- Wiki citation validation checks only whether the UUID was among the supplied chunks, not whether that chunk supports the paragraph. [compile.rs:743](/Users/dev/Projects/engram-cli/crates/engram-cli/src/commands/compile.rs:743)
- Claims store `evidence_level` and confidence, but the recall candidate discards both and retains only content and citations. [retrieval.rs:863](/Users/dev/Projects/engram-cli/crates/engram-cli/src/retrieval.rs:863)

I would add a derivation/support graph: every fact, claim, entity hypothesis, relation, and wiki sentence links to exact source spans with `supports` or `refutes`, polarity, deriver/version, and verification status. State must never substitute an arbitrary chunk. Citation existence should be reported as addressability; evidential support requires a separate, stronger check.

5. **The confidence scalar is a category error, not merely uncalibrated.**

The code candidly admits that its constants are chosen rather than measured. [state.rs:43](/Users/dev/Projects/engram-cli/crates/engram-cli/src/commands/state.rs:43) More importantly, it adds quantities that are not interchangeable: extraction fidelity, source class, repeated confirmation, temporal age, and contest status. It then calls the result calibrated.

Its staleness term uses `valid_from`, meaning “when the state began,” rather than “when evidence was last observed or verified.” [state.rs:392](/Users/dev/Projects/engram-cli/crates/engram-cli/src/commands/state.rs:392) I recorded a user statement today confirming continuous metformin use since 2010. Engram reduced confidence from `0.95` to `0.85` because the medication had been true for a long time. Duration is not evidential staleness.

Return an epistemic vector instead:

- extraction fidelity;
- source authority;
- last supported/verified time;
- independent supporting sources;
- contradiction status;
- temporal applicability;
- coverage/completeness.

Do not emit a composite probability until it is calibrated per relation and use case against outcomes. Publishing the basis is genuinely good; encouraging an agent to compare the resulting scalar is not.

The open-interval view, explicit open questions, proposed-versus-confirmed distinction, and deterministic auditability are all solid foundations. But today they improve inspectability, not reasoning. The two changes I would prioritize first are removing agent self-confirmation from procedural memory and adding relation-level answerability/coverage to `state`.


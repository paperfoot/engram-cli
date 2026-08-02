## Verdict

Retrieval is not “already elite” in the sense a frontier agent needs. It has a competent passage-ranking kernel, but it does not reliably distinguish:

1. “I searched.”
2. “I found relevant evidence.”
3. “The store knows the answer.”
4. “The store has evidence that the answer is absent.”

That missing distinction is the most important problem.

### 1. Critical — the absence certificate certifies scope traversal, not absence

Default recall embeds the query, scores every compatible chunk, and admits the highest-ranked chunks without any relevance cutoff. Therefore, when a scope contains at least one chunk, semantic `no_match` is effectively impossible. [`hybrid_recall`](/Users/dev/Projects/engram-cli/crates/engram-cli/src/retrieval.rs:794) and [`absence_reason`](/Users/dev/Projects/engram-cli/crates/engram-cli/src/read_surface.rs:281) are explicit about the lack of a cutoff.

It was correct not to fabricate a threshold. The mistake is continuing to call the result an “absence certificate.” It proves that the scope contained zero candidates, not that the requested fact is absent.

This becomes actively unsafe in `research`: `ready_for_synthesis` is simply `cited_results > 0`. [`research.rs`](/Users/dev/Projects/engram-cli/crates/engram-cli/src/commands/research.rs:212) I reproduced:

```text
store: "Alice prefers green tea."
query: "What is Bob's blood type?"

verdict: results
result: "Alice prefers green tea."
ready_for_synthesis: true
```

The personal benchmark confirms the general mechanism: all six negative controls returned ten results, abstention was `0.0`, and every top score was `1.0`.

There is another hole: lexical, claim, and wiki search failures are converted into empty runs with only a tracing warning, while the response still presents itself as hybrid retrieval. [`retrieval.rs`](/Users/dev/Projects/engram-cli/crates/engram-cli/src/retrieval.rs:817) A certificate cannot be trustworthy without saying which retrieval components actually succeeded.

What I would do instead:

- Rename the current object `search_receipt` or `scope_receipt`.
- Reserve `no_match` for a calibrated null-relevance decision.
- Add per-source coverage: attempted, succeeded, failed, candidate count, index version/freshness.
- Calibrate abstention separately for model/profile, query class, result kind, and corpus scale. RRF scores cannot support one global threshold.
- Issue a genuine absence certificate only for closed-world reads that exhaust a defined projection, such as “no open `blood_type` fact for resolved subject X.”
- Make `ready_for_synthesis` require relevant cited evidence above the calibrated bar, not merely any citation.

### 2. Critical — recall ranks immutable text, not epistemic state

Facts, intervals, and decisions are not retrieval candidates. The candidate vocabulary is dense, lexical, claim, wiki, entity, graph, and reranker—there is no fact/current-state source. [`RetrievalSource`](/Users/dev/Projects/engram-cli/crates/engram-core/src/types.rs:297)

A returned chunk carries no event time, observation time, validity interval, or current/historic/contested status. `observed_at` exists in provenance but is intentionally left unset by recall. [`read_surface.rs`](/Users/dev/Projects/engram-cli/crates/engram-cli/src/read_surface.rs:145)

Closed facts are handled by an opportunistic sideband scan after ranking. That scan:

- is not attached to the result it qualifies;
- is absent from `answer_context`;
- examines only 20 facts;
- matches query terms against fact subjects and objects, not predicates. [`facts_matching_terms`](/Users/dev/Projects/engram-cli/crates/engram-storage/src/sqlite.rs:4401)

I reproduced two consequences after explicitly adjudicating 500 mg → 1000 mg:

```text
query: "What dose of metformin does Ana currently take?"

rank 1: "Ana takes metformin 500 mg daily."       # retired
rank 2: "Ana now takes metformin 1000 mg daily."
conflicts: correctly says 500 mg was superseded
```

With the normal first-person query, the warning disappears:

```text
query: "What medication am I currently taking?"

results: both old and new chunks
conflicts: []
```

Temporal retrieval also ignores the new intervals. A fact valid from January and still open was returned as `no_match` for “Where was Ana living in February?” because `--since/--until` filters the source memory’s event time and explicitly excludes derived layers. [`retrieval.rs`](/Users/dev/Projects/engram-cli/crates/engram-cli/src/retrieval.rs:746)

The unused capability is bi-temporal retrieval:

- “What is true now?” → open intervals.
- “What was true on date T?” → valid-time containment.
- “What did we believe on date T?” → transaction/observation time.
- Adjudicated/rejected facts → suppress or clearly label their source text for current-state queries.

I would make facts/current-state cards first-class candidates with supporting chunk provenance. Every raw result should be joined to relevant fact state and carry `epistemic_status`, `valid_from`, `valid_until`, and successor/decision information. Historic text can remain retrievable, but it must not appear as unqualified fresh evidence for a current-state query.

### 3. High — the 100k scale cliff exists at both ends of retrieval

Every recall loads every in-scope embedding and its full content from SQLite into a `Vec`. [`sqlite.rs`](/Users/dev/Projects/engram-cli/crates/engram-storage/src/sqlite.rs:1884) It then computes cosine similarity for all chunks and sorts the complete score vector. [`retrieval.rs`](/Users/dev/Projects/engram-cli/crates/engram-cli/src/retrieval.rs:769)

At 100,000 chunks with the 1,536-dimensional production embedding, decoded vectors alone occupy roughly 586 MiB. [`gemini.rs`](/Users/dev/Projects/engram-cli/crates/engram-embed/src/gemini.rs:13) Content strings, metadata, allocator overhead, SQLite pages, and concurrent readers are additional. A store with 100,000 memories will normally have more than 100,000 chunks.

The context-side bound is also fictional. `hybrid_recall` already returns at most `top_k`; recall then guarantees all `top_k` results before consulting the layer budget. [`recall.rs`](/Users/dev/Projects/engram-cli/crates/engram-cli/src/commands/recall.rs:130) A test deliberately pins identity and deep layers to the same result set and calls the budget a “reporting knob.” [`wave05_daemon_parity.rs`](/Users/dev/Projects/engram-cli/crates/engram-cli/tests/wave05_daemon_parity.rs:438)

My probe requested `--layer identity`, which advertises 50 tokens, and received 455 estimated tokens.

I would replace this with:

- A local ANN index returning IDs only, partitionable by KB, diary, and temporal filters.
- Exact scoring only over the ANN/FTS candidate union.
- Separate per-source retrieval depth from the final rerank pool.
- Lazy content/provenance fetch after candidate selection.
- A real hard context packer. If `top_k` conflicts with the token limit, return a named truncation reason rather than pretending both constraints were honored.
- Layer-specific representations: compact current fact cards for identity/critical, passages for topic, expanded history for deep.
- A 1k/10k/100k distractor-growth benchmark tracking recall, latency, peak RSS, and concurrent-reader amplification.

### 4. High — `bench personal` is a useful smoke fixture mistaken for quality evidence

The current suite has 53 memories and 43 queries: 37 scored queries plus six negatives. [`personal.rs`](/Users/dev/Projects/engram-cli/crates/engram-bench/src/personal.rs:1) Each scoped KB contains only 11–18 memories, while default `top_k` is 10. R@10 therefore often means returning almost the entire KB.

More importantly:

- The deterministic embedder describes itself as “not meaningful semantically.” [`stub.rs`](/Users/dev/Projects/engram-cli/crates/engram-embed/src/stub.rs:1)
- Seeding creates only memories and chunks—no facts, claims, wiki, entities, intervals, decisions, or supersessions. [`personal.rs`](/Users/dev/Projects/engram-cli/crates/engram-bench/src/personal.rs:1186)
- Negative controls are excluded from headline retrieval metrics. [`personal.rs`](/Users/dev/Projects/engram-cli/crates/engram-bench/src/personal.rs:1520)
- The integration test checks that metrics are well-formed, not that quality clears a floor. [`cli.rs`](/Users/dev/Projects/engram-cli/crates/engram-cli/tests/cli.rs:3127)
- The “paraphrase dedup” category rewards recovering both duplicate memories; it has no redundancy penalty.

LongMemEval’s 0.91 R@1 is legitimate evidence for a narrower claim: the chunk-only dense+FTS+RRF kernel performs well over 48 sessions per question. But it runs through a separate implementation, builds a fresh in-memory store per question, and never exercises production mixed-layer retrieval, abstention, longitudinal interference, or truth-state changes. [`longmemeval.rs`](/Users/dev/Projects/engram-cli/crates/engram-bench/src/longmemeval.rs:199)

Keep the deterministic suite, but call it a regression smoke test. Add a production benchmark with:

- realistic paraphrase and indirect-reference queries;
- negative queries scored by abstention precision/recall;
- repeated corrections and current/as-of questions;
- rejected and superseded facts whose raw chunks remain;
- mixed chunks, claims, wiki, facts, and entities;
- increasing distractor sets through 100k+;
- stale-hit rate, current-truth accuracy, redundancy, calibration error, latency, and RSS;
- hard CI floors rather than “metric is between zero and one.”

### 5. High — fusion combines incomparable objects and then exposes an incomparable score

Chunks receive dense and lexical runs. Claims and wiki pages receive lexical-only runs. Entities are exact-name matches ranked by mention count. Relations are graph-expansion leads. All of these independently identified objects compete in one RRF list.

RRF discards every raw score and adds only `1/(k + rank)`. [`fusion.rs`](/Users/dev/Projects/engram-cli/crates/engram-core/src/fusion.rs:28) This gives raw chunks two possible votes while derived rows usually get one, fragments a claim from its supporting chunk instead of consolidating their evidence, and treats a wiki page, a person-name aggregate, and a source passage as interchangeable answer units.

The public `score` then changes meaning by profile:

- Cohere: model relevance score.
- Passthrough: an invented descending position score, with the first result always `1.0`. [`passthrough.rs`](/Users/dev/Projects/engram-cli/crates/engram-rerank/src/passthrough.rs:14)

That is why the offline negative controls all look maximally confident.

I would route before fusion:

- Current/as-of fact queries → interval fact cards plus supporting passages.
- Evidence questions → cited claims/passages only.
- Explore queries → entities and graph leads in a separate channel, not mixed into evidence top-k.
- Wiki synthesis queries → wiki pages with freshness and evidence coverage.

Fuse at a semantic answer unit, linking a claim to its parent chunk and fact rather than treating each row ID as independent. Preserve `dense_similarity`, `bm25`, `rrf_score`, and `rerank_score` separately. Emit a generic relevance/confidence value only after calibration; passthrough should return no score rather than a fabricated `1.0`.

The genuinely good pieces are worth keeping: the untrusted-content envelope, typed provenance, and refusal to invent a threshold. But they are trustworthy plumbing around a retrieval contract that still overstates what it knows.

I would treat findings 1 and 2 as blockers for marketing `no_match` as evidence of absence or recall as current truth; finding 3 is the six-month operational blocker.


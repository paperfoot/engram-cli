# Recall latency and quality, October 2026

Goal: a phone assistant recalls through `engramd` in well under a second,
with no loss in retrieval quality. Aggregate numbers only; no queries or
memory text from any personal store appear here.

## Latency

A copy of a real store (17,750 chunks, ~471 characters each), 20 neutral
queries x 2 rounds, `benchmarks/recall_latency.py`. Milliseconds, p50 / p95.
Provider times vary run to run by about ±50 ms at p50.

| Surface | Before | After |
|---|---|---|
| CLI recall, total | 1376 / 2463 | 912 / 1130 |
| Daemon, warm (before: no relevance judge) | 881 / 1053 | 800 / 959 |
| Daemon, after 100 s idle | 1136 / 1424 | 953 / 1104 |

Stages, CLI, p50 (p95 where it matters), before -> after:

| Stage | Before | After |
|---|---|---|
| Gemini query embedding | 347 | 372 |
| Cohere rerank-v5.0-pro (200 docs) | 482 | 488 |
| Relevance judge | 323 (p95 1504, the deadline) | 345 (p95 465), alongside the rerank |
| Chunk scan, dense, BM25, fuse, assembly | 34 | 33 |

What changed: providers share one HTTP client per process and the daemon
drives requests on its own runtime, so connections survive between recalls;
the judge scores the whole rerank pool in parallel with the rerank instead of
walking it in sequential batches afterwards; the CLI opens Cohere and
TypeSafe connections while the query embeds; the daemon opens connections at
start and refreshes them every 45 s.

The floor is two network calls in sequence: the query embedding (~310 ms
warm) and the rerank (~400 ms warm). A p50 of 500 ms needs one of them gone
or much faster.

Rejected, with numbers:
- Overlapping the embedding with the scan and BM25: at most 15 ms, inside
  the noise, and it would pay for an embedding a mixed-metadata store then
  refuses.
- Cohere rerank-v5.0-fast: slower than pro here (660 vs ~420 ms p50).
- Smaller, more parallel judge requests: 4 x 64 questions took 249 ms,
  8 x 25 took 264 ms; JEV latency barely grows with questions per request
  (10: 139 ms, 64: 190 ms).

## Quality: LongMemEval through the full pipeline

311 questions, frozen per-question stores, Gemini + Cohere + JEV as a user's
recall runs them (`benchmarks/longmemeval_retrieval.py`); answers by
gpt-6.1-terra, judged with LongMemEval's own templates by claude-haiku-5.5.
Evidence recall@k: share of gold evidence sessions among the first k results.

| Arm | recall@5 | recall@10 | Answer accuracy |
|---|---|---|---|
| Judge on, sequential walk, 1.5 s deadline (old default) | 0.848-0.853 | 0.865-0.869 | 74.0-74.3% |
| Judge on, sequential walk, no deadline | 0.835 | 0.846 | 73.3% |
| Judge on, judged alongside the rerank | 0.835-0.843 | 0.846-0.852 | 71.7-74.6% |
| Judge off | 0.952 | 0.986 | 87.1-88.4% |
| Judge shadow (new default) | 0.952 | 0.986 | 87.8% |

- Noise floor: two runs of one arm return identical result sets on 252 of
  311 questions and differ by 13 vs 12 correct answers.
- Judging alongside the rerank matches the sequential walk within that floor
  (243-251 identical result sets); the old default's higher recall came from
  the deadline cutting the walk short on more than half the recalls.
- Shadow returned the judge-off result set on all 311 questions.
- Dropping below the floor costs about 14 points, almost all on temporal
  questions (84% -> 56%), whose evidence is spread over passages that do not
  answer alone. Abstention questions score 93% either way. Hence the shadow
  default.

## JEV as the ranker (removed)

Ranking the pool by the judge's P(answers) instead of Cohere looked good on
LongMemEval's small stores (recall@10 0.993, accuracy 91.0% in shadow) and
saved ~60 ms at p50. On the 17,750-chunk store it did not hold: 40 real
queries, top 5, a blind pairwise judge (random A/B order) preferred the
Cohere page 26 to 9 with 5 ties (two-sided sign test p = 0.006), and the two
top-5 sets shared 1.65 of 5 passages on average. The experiment was removed.

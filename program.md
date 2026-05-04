# engram v2 — Autoresearch Research Direction

## Current goal (2026-04-09)

**Beat Cohere's 78.8% LoCoMo-full-mini end-to-end QA accuracy** — locally, reproducibly, with
a reranker path we own. Secondary goal: restore per-question latency to ≤3s/q.

The target eval is a **deterministic 50-question slice** of LoCoMo-full-mini (first 50 in
sample order). The Cohere baseline on the same slice is 71.1% (27/38 from the partial MLX
run mapped to the same index range). The full 1542-question Cohere run scored 78.8%
(`benchmarks/locomo-full-mini-cohere.json`).

## The confound we're untangling

The partial MLX bench crashed at 38/1542 with 47.4% accuracy and 17.0s/q — 24 points below
Cohere on the same 38 questions and 6× slower. **BUT** the two runs used different embed
models:

| run | timestamp | embed model | reranker | acc (slice) | latency |
|---|---|---|---|---:|---:|
| Cohere | 2026-04-09 01:56 | gemini-embedding-001 (OLD) | Cohere rerank-v3.5 | 71.1% | 2.7s/q |
| zerank2-mlx (crashed) | 2026-04-09 04:24 | gemini-embedding-2-preview (NEW) | zerank-2-mlx | 47.4% | 17.0s/q |

We don't know yet which change is the killer. The loop's first four experiments isolate it.

## Iteration strategy (Karpathy ordering)

### Phase 0 — confound isolation (MUST do first)

| # | embed model | reranker | hypothesis |
|---|---|---|---|
| 1 | gemini-embedding-001 | cohere | BASELINE: re-confirm 71.1% on the 50-q slice |
| 2 | gemini-embedding-001 | zerank2 (MLX) | Isolate reranker: if accuracy holds, MLX port is fine |
| 3 | gemini-embedding-2-preview | cohere | Isolate embed: if accuracy tanks, embed-2 is broken |
| 4 | gemini-embedding-2-preview | zerank2 (MLX) | Matches crashed run baseline |

After these four runs we know which component is the regression. Then:

### Phase 1 — hyperparameters (cheap, high signal)

- RRF k constant (currently 60; sweep 20..120)
- Dense retrieval top-N into fusion (currently 50; sweep 20..100)
- Rerank top-K into answerer (currently 5; sweep 3..10)
- Answerer context format (current: `[session N — id]\n{text}`)

### Phase 2 — embed/rerank swaps

- Gemini Embed 2 (if Phase 0 says it's good)
- Pinned embed-001 (if Phase 0 says preview is bad)
- Replace zerank-2 with a newer/stronger local reranker (see "candidate local rerankers" below)

### Phase 3 — architecture changes

- Section-aware chunking of LoCoMo sessions (currently whole-session chunks)
- Query expansion before embed (LLM-rewritten query)
- Two-stage rerank (cheap → expensive)
- Answerer switch to gpt-5.4 with longer context window

## Candidate local rerankers (max 6 months old per project constraint)

Current date: 2026-04-09. Max-6-months cutoff: **2025-10-09**. To research in Phase 2 if
Phase 0 implicates the reranker:

- ZeroEntropy `zerank-2` (current) — released ~Sep 2025 (borderline, model card claims SOTA
  on biomedical, not necessarily LoCoMo-style multi-turn)
- Qwen3-Reranker-4B / 8B — check release date
- mxbai-rerank-v3 — if it exists
- jina-reranker-v3 — check release date
- BAAI bge-reranker-v3 — check release date
- Gemma-rerank (Google) — check

Any candidate must:
1. Have a clean MLX or torch path for Apple Silicon
2. Fit in 64 GB unified memory with room for the answerer
3. Pass a correctness gate vs its own HF reference on ≥20 pairs before shipping

## Eval mechanics

- `scripts/experiment.vars` — the knobs (autoresearch target_file)
- `scripts/eval_locomo_slice.sh` — runs the bench, prints accuracy on stdout
- Checkpoint per-question JSONL at `benchmarks/ar/locomo-slice-<hash>-<ts>.json`
- Slice size: 50 questions (ENGRAM_LOCOMO_LIMIT). Validate wins at 200q before declaring a
  milestone. 50q has ±13% 95% CI so require ≥2pt deltas to trust the signal.

## Reward-hacking watch

- LoCoMo recall_at_5 / MRR are always 0 because the dataset has no `answer_session_ids`.
  Only the LLM-judged `correct` verdict is meaningful. Any experiment that improves R@5
  or MRR without improving accuracy is noise.
- The 50-q slice is stratified by the first-in-dataset sample order, not randomly sampled.
  Results on it may over- or under-represent easier categories. Spot-check category
  distribution after each run.
- Don't game latency by reducing retrieval quality. Latency is a secondary metric — wins
  must not cost ≥1pt of accuracy.

## Prior autoresearch milestones (different loop, for context)

- LongMemEval S 500-q full run: R@5 = 0.99 with plain hybrid Gemini + FTS + RRF,
  no rerank. Retrieval is not the bottleneck.
- Mini bench: saturated at R@1 = 1.0 since run 4 (architectural: added Gemini dense).

## When stuck

1. Re-read this file
2. `autoresearch review` → pipe to Codex (gpt-5.4, xhigh) and Gemini (auto) for second opinions
3. Try a completely different lever than the last 5 discards
4. If the eval slice saturates, expand to 200q or the full 1542

# engram v2 — Autoresearch Research Direction

## Goal

Match or beat MemPalace's 98.4% R@5 on LongMemEval held-out split, then push it higher.

## Current baseline (Phase 1)

Run on the inline mini benchmark (10 questions, 20 chunks, FTS5 only):

- **R@1 = 0.80** (8/10 correct as top result)
- **R@5 = 1.00**
- **MRR = 0.90**

The two failing questions are pure-semantic queries with no keyword overlap:
1. "How does eating fewer calories than normal affect animal lifespan?" → caloric restriction
2. "Which NAD+ booster has been shown to raise the cofactor in elderly humans?" → nicotinamide riboside

These are exactly the cases dense retrieval + reranking should solve.

## What's actually wired up

- SQLite source of truth + FTS5 lexical search
- Section-aware chunking stub (currently naive paragraph split)
- Embedder trait + Gemini Embed 2 client + offline stub
- Reranker trait + Cohere Rerank 4 Pro client + passthrough
- LongMemEval question/dataset structures
- agent-cli-framework patterns: agent-info, JSON envelope, semantic exit codes

## What is NOT wired up yet (Phase 2 priorities)

These are the highest-leverage changes the loop should attack:

1. **Plug Gemini Embed 2 into ingestion + recall** — biggest single quality lift
2. **Wire LanceDB as the vector store** — currently embeddings have no home
3. **RRF fusion of dense + lexical** — `engram-core::fusion` is implemented but not called
4. **Cohere reranking on top-50 candidates** — second-biggest quality lift
5. **Real LongMemEval dataset download + harness** — replaces the mini set as primary eval

## Hyperparameter targets (Karpathy ordering: hyperparameters first)

1. RRF `k` constant (current: 60, range: 1..120)
2. Top-N for dense retrieval (current: 50, range: 10..200)
3. Top-N for FTS retrieval (current: 50, range: 10..200)
4. Rerank top-N (current: 20, range: 5..50)
5. Stopword list size for FTS query builder
6. Token length threshold for FTS query (current: <3 chars dropped)
7. Embedding output dimensionality (Gemini supports 768/1536/3072)

## Architecture experiments (after hyperparams plateau)

1. Section-aware chunking that preserves headers in chunk metadata
2. Query expansion via synonym dictionary (gene/protein aliases especially)
3. Two-stage ingestion: abstracts first, full text second (fastbmRAG idea)
4. Memory layer L1 AAAK compression (port from MemPalace)
5. Temporal validity filtering on retrieval

## Things to avoid

- LLM triple extraction at ingest time — too slow, too noisy, low ROI per Codex review
- Personalized PageRank as primary retriever — over-engineered for v1
- 3 separate edge types as core infrastructure — defer to Phase 4
- Caching results in a way that lets the metric memorize the test set

## Reward hacking to watch for

The mini bench is only 10 questions. If R@1 hits 1.0, do NOT trust it on its own
— validate against the real LongMemEval before declaring victory. Rotate test
data, add held-out questions, and always pair retrieval improvements with
correctness gates.

## When stuck (5+ consecutive discards)

1. Re-read this file
2. Run `autoresearch review` and pipe to Codex/Gemini
3. Try the opposite of what's been failing
4. Switch from hyperparameter tuning to an architecture change (or vice versa)
5. Check whether the bottleneck has shifted — `R@1` may be saturated but
   `mean_latency_ms` might be the new target

## Cross-references

- Spec: `docs/superpowers/specs/2026-04-07-engram-v2-design.md`
- LongMemEval: https://github.com/xiaowu0162/LongMemEval
- agent-cli-framework: https://github.com/199-biotechnologies/agent-cli-framework
- HippoRAG2: https://github.com/OSU-NLP-Group/HippoRAG

# engram v2 — Autoresearch Research Direction

## Goal

Match or beat MemPalace's 98.4% R@5 on LongMemEval (S split, held-out questions), then push higher.

## Current state — end of bootstrap session

### Mini benchmark (10 hand-written questions, runs in <1 s)

| mode | R@1 | R@5 | MRR | notes |
|---|---|---|---|---|
| `mini-fts` (FTS5 only) | 0.90 | 1.00 | 0.95 | After expt#1 stopword tuning |
| `mini` hybrid_stub | 0.50 | — | — | Offline CI baseline |
| `mini` hybrid_gemini | **1.00** | **1.00** | **1.00** | Gemini Embed 2 + RRF fusion (saturated) |

### LongMemEval S split (500 questions, ~48 sessions/q, 96% distractors)

| mode | R@1 | R@5 | R@10 | MRR | notes |
|---|---|---|---|---|---|
| Stub embedder, first 10 questions | 0.20 | 0.50 | 0.80 | 0.31 | Random-ish baseline |
| **Gemini, first 10 questions** | **TBD** | **TBD** | **TBD** | **TBD** | Free-tier quota exhausted; resume when reset |
| **Gemini, full 500** | **TBD** | **TBD** | **TBD** | **TBD** | Target ≥ 0.984 R@5 (MemPalace parity) |

### What is wired up

- Cargo workspace, 8 crates, 17 unit tests pass
- agent-cli-framework patterns: `agent-info`, JSON envelope, semantic exit codes, `skill install`
- SQLite + FTS5 source of truth with Porter stemming
- Gemini Embed 2 client with both `embedContent` and `batchEmbedContents`, token-budgeted batching, per-text truncation under 2,048 tokens
- Cohere Rerank 4 Pro client (untested — `COHERE_API_KEY` not set in this env)
- Stub embedder for offline / CI runs (deterministic)
- Reciprocal Rank Fusion with deterministic tiebreak (UUID v5 + chunk_id ordering)
- LongMemEval Oracle + S split loaders, dedup of duplicate session IDs
- LongMemEval runner: in-memory store per question, batch-embed haystack, hybrid retrieval, R@k metrics
- Embedding cache on disk per embedder name so iterative runs hit cache after warmup

### What is NOT wired up yet (Phase 2 priorities)

The autoresearch loop should attack these in order:

1. **Run the full LongMemEval S split with Gemini once quota resets** — establish the honest baseline, record it, compare to MemPalace's 98.4% R@5
2. **Cohere Rerank 4 Pro on top-50 candidates** — biggest single quality lift after dense retrieval
3. **Section-aware chunking** that preserves headers in chunk metadata
4. **LanceDB** as the persistent vector store (currently in-memory HashMap per question)
5. **Memory layers (L0–L3)** — actually use them in `recall` output, not just types
6. **AAAK compression port from MemPalace** — replace `IdentityCompressor`

### Hyperparameter targets (Karpathy ordering: hyperparameters first)

1. RRF `k` constant (current: 60, range: 1..200)
2. Top-N for dense retrieval (current: 50, range: 10..200)
3. Top-N for FTS retrieval (current: 50, range: 10..200)
4. Rerank top-N (current: 20, range: 5..50)
5. Stopword list size for FTS query builder
6. Token length threshold for FTS query (current: <3 chars dropped)
7. Embedding output dimensionality (Gemini supports 768/1536/3072)

### Reward hacking watch

- The mini bench is **saturated** at R@1 = 1.0 with hybrid_gemini. Do NOT keep tuning against it — switch the loop's eval to LongMemEval S as soon as quota allows.
- The Oracle split is degenerate (haystack == answer set). Do not use it for retrieval evaluation; it tests answer generation only.
- Always run determinism check (`for i in 1 2 3; do ./target/debug/engram bench mini --json | jq -r .data.recall_at_1; done`) before declaring a win — earlier 1.0 R@1 results were caused by random UUIDs and non-deterministic HashMap iteration order. We caught and fixed two such bugs already.

### When stuck (5+ consecutive discards)

1. Re-read this file
2. Run `autoresearch review` and pipe to Codex/Gemini for cross-model second opinions
3. Try the opposite of what's been failing
4. Switch from hyperparameter tuning to an architecture change
5. Check for bottleneck shifts (R@1 plateau may mean it's now a latency or cost problem)

### Cross-references

- Spec: `docs/superpowers/specs/2026-04-07-engram-v2-design.md`
- LongMemEval: https://github.com/xiaowu0162/LongMemEval
- LongMemEval-cleaned: https://huggingface.co/datasets/xiaowu0162/longmemeval-cleaned
- agent-cli-framework: https://github.com/199-biotechnologies/agent-cli-framework
- HippoRAG2: https://github.com/OSU-NLP-Group/HippoRAG
- Gemini Embedding API: https://ai.google.dev/gemini-api/docs/embeddings

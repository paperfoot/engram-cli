# Engram v2 — Design Spec

**Author:** Boris Djordjevic, 199 Biotechnologies
**Date:** 2026-04-07
**Status:** Approved for implementation

---

## 1. Purpose

A single Rust CLI that gives AI agents persistent, agent-native memory across conversations. Handles two workloads with the same engine:

1. **Scientific knowledge** — ingest research papers, surface multi-hop connections
2. **Personal/agent memory** — facts, preferences, decisions, conversations

It replaces engram v1 (TypeScript MCP server) and aims to match or exceed MemPalace (Python, 98.4% R@5 on LongMemEval held-out split) while being faster, having a lower token footprint, and exposing itself as a CLI rather than an MCP server.

## 2. Non-goals

- No web UI for humans
- No MCP server (CLI is the interface — see [agent-cli-framework](https://github.com/199-biotechnologies/agent-cli-framework))
- No separate Qdrant/Neo4j/external services — local-first, single binary
- No interactive prompts — fully non-interactive, agent-native
- No mandatory cloud APIs — local fallbacks must work

## 3. The Five Things That Matter

After triple-checking with GPT-5.4 and Gemini, the components that determine v1 quality are:

1. **Hybrid retrieval** — dense + lexical fusion + reranking (the boring foundation that wins)
2. **Section-aware chunking** — preserve document structure, never flatten
3. **AAAK-style compression** — lossless 30x compression for context efficiency
4. **Memory layers (L0-L3)** — tiered context loading per call
5. **LongMemEval benchmark harness** — every change measured against ground truth

Everything else (PPR graph traversal, LLM triple extraction, exotic chunking strategies) is deferred until the foundation beats MemPalace.

## 4. Architecture

### 4.1 Cargo workspace

```
engram-2/
├── crates/
│   ├── engram-core/         # Pure logic, no I/O — types, fusion, compression
│   ├── engram-storage/      # SQLite + LanceDB + FTS5
│   ├── engram-embed/        # Gemini Embed 2 + local fallback (trait)
│   ├── engram-rerank/       # Cohere Rerank 4 Pro (trait)
│   ├── engram-ingest/       # Mining modes: papers, conversations, repos, general
│   ├── engram-graph/        # Deterministic edges first; LLM triples optional
│   ├── engram-bench/        # LongMemEval harness
│   └── engram-cli/          # Binary — agent-cli-framework patterns
├── benches/                 # Criterion micro-benchmarks
├── tests/                   # Integration tests
└── data/longmemeval/        # Benchmark data (gitignored)
```

### 4.2 Storage layout

| Store | Role | Path |
|---|---|---|
| SQLite (rusqlite) | Source of truth: memories, chunks, edges, metadata, FTS5 | `~/.local/share/engram/db.sqlite` |
| LanceDB | Embedded vector index, rebuildable from SQLite | `~/.local/share/engram/vectors/` |
| FTS5 (in SQLite) | Lexical search for gene names, acronyms, citations | (in db.sqlite) |
| Config | TOML | `~/.config/engram/config.toml` |
| Cache | API response cache | `~/.cache/engram/` |

### 4.3 Retrieval pipeline (v1 baseline)

```
query
  ├─→ Gemini Embed 2 (RETRIEVAL_QUERY mode)
  │     └─→ LanceDB ANN search (top 50)
  ├─→ FTS5 BM25 search (top 50)
  └─→ entity matcher (exact + alias)

  → fusion (Reciprocal Rank Fusion, k=60)
  → Cohere Rerank 4 Pro (top 50 → top 10)
  → graph expansion from top hits (1-hop, optional)
  → memory layer assembly (L0/L1/L2/L3 budget)
  → AAAK compression (optional, --compact flag)
  → JSON envelope output
```

This is the **baseline**. The autoresearch loop iterates on every step.

### 4.4 CLI surface

Following [agent-cli-framework](https://github.com/199-biotechnologies/agent-cli-framework) patterns. Every command has `--json`, auto-detects piped output, and uses semantic exit codes.

| Command | Purpose |
|---|---|
| `engram remember <text>` | Store a memory with optional importance/tags |
| `engram recall <query>` | Retrieve relevant memories (hybrid search) |
| `engram ingest <path>` | Mine papers/repos/conversations/general content |
| `engram entities [list\|show <name>]` | Browse the entity graph |
| `engram forget <id>` | Soft-delete a memory |
| `engram edit <id>` | Update memory content/importance |
| `engram export [--format json]` | Backup |
| `engram import <file>` | Restore |
| `engram bench [longmemeval]` | Run benchmark, output metrics |
| `engram config show\|set\|check` | Manage config |
| `engram update [--check]` | Self-update from GitHub Releases |
| `engram skill install` | Deploy skill signpost to ~/.claude, ~/.codex, ~/.gemini |
| `engram agent-info` | Self-describing JSON manifest |

### 4.5 JSON envelope

```json
{
  "version": "1",
  "status": "success",
  "data": { ... },
  "metadata": {
    "elapsed_ms": 342,
    "embedding_model": "gemini-embed-2",
    "reranker": "cohere-rerank-4-pro",
    "candidates_considered": 100,
    "results_returned": 10
  }
}
```

Errors go to stderr with semantic exit codes:
- 0 = success
- 1 = transient (IO/network — agent retries)
- 2 = config error (agent fixes setup)
- 3 = bad input (agent fixes args)
- 4 = rate limited (agent backs off)

## 5. What we steal from MemPalace

| Innovation | How we adopt it |
|---|---|
| **AAAK compression** | Port the dialect to Rust as `engram-core::compress`. Verify lossless round-trip on test corpus. |
| **Memory layers (L0-L3)** | `engram-core::layers` — L0 = identity (~50 tokens), L1 = critical facts AAAK-encoded (~120 tokens), L2 = on-demand topic context, L3 = deep search. Every recall returns budgeted output. |
| **Mining modes** | `engram-ingest::{papers, conversations, repos, general}` — first-class subcommands. |
| **Temporal validity** | Every fact has `valid_from` / `valid_until`. Queries can ask "what was true on date X?". |
| **Specialist agent diaries** | `engram --diary <name>` namespace flag — separate memory contexts per agent. |
| **Lossless verbatim storage** | Original chunks always preserved; compression is additive, not destructive. |

## 6. What we improve over MemPalace

| Improvement | Why |
|---|---|
| **Rust single binary** | 10-100x faster startup, zero deps, `cargo install` |
| **Agent-CLI vs MCP** | 32x cheaper tool discovery (1.4K vs 44K tokens) |
| **Gemini Embed 2** | #1 MTEB, vs ChromaDB defaults |
| **FTS5 lexical** | Gene names, acronyms, citations — MemPalace doesn't have this |
| **Cohere Rerank 4 Pro** | Adds precision lift on top of fusion |
| **LanceDB embedded** | No server, Rust-native, rebuildable from SQLite |
| **Section-aware paper chunking** | Purpose-built for scientific papers |

## 7. Benchmarking strategy

Every change measured against [LongMemEval](https://github.com/xiaowu0162/LongMemEval) — the same benchmark MemPalace uses.

**Target metrics:**
- R@1 ≥ MemPalace
- R@5 ≥ 98.4% (MemPalace's hybrid_v4 held-out result)
- R@10 ≥ 99.8% (MemPalace's reported number)
- Recall latency p50 < 100ms (vs MemPalace's Python overhead)

**Baseline measurement:** First experiment is naive dense retrieval only. Every subsequent experiment must beat or match this on at least one metric without regressing the others.

**Anti-gaming:** Use the held-out 450-question split MemPalace publishes, not the contaminated full 500. Rotate test data when possible. Quality gates alongside speed metrics.

## 8. Development methodology — autoresearch loops

This project uses [autoresearch](https://github.com/199-biotechnologies/autoresearch) for autonomous component optimization. Each major component gets its own loop:

| Component | Target file | Metric | Eval command |
|---|---|---|---|
| Fusion algorithm | `crates/engram-core/src/retrieval/fusion.rs` | R@5 on LongMemEval | `engram bench longmemeval --json` |
| Chunking strategy | `crates/engram-ingest/src/papers.rs` | R@5 on LongMemEval | `engram bench longmemeval --json` |
| Rerank top-k | `crates/engram-core/src/retrieval/rerank.rs` | R@5 / latency | `engram bench longmemeval --metrics all` |
| Memory layer budgets | `crates/engram-core/src/layers.rs` | tokens / R@5 | `engram bench longmemeval --layers` |
| AAAK compression ratio | `crates/engram-core/src/compress.rs` | bytes saved / round-trip fidelity | `cargo test compress::roundtrip` |

Every loop:
1. Records baseline
2. Iterates one atomic change at a time
3. Keeps wins, discards losses, commits before each eval
4. Stops when 5+ consecutive discards (signals local minimum, requires human/cross-model review)

## 9. Crate dependencies

| Crate | Version | Purpose |
|---|---|---|
| clap | 4.5+ | CLI parsing (derive macro) |
| tokio | 1.x | Async runtime |
| reqwest | 0.12+ | HTTP client (rustls-tls, no OpenSSL) |
| rusqlite | 0.32+ | SQLite bindings (bundled feature) |
| lancedb | 0.15+ | Embedded vector database |
| serde / serde_json | 1.0 | Serialization |
| thiserror | 2.x | Error enum |
| figment | 0.10+ | Config layering |
| comfy-table | 7.x | Human output |
| owo-colors | 4.x | Terminal colors (auto-disable when piped) |
| directories | 5.x | XDG paths |
| self_update | 0.43+ | GitHub releases |
| tracing / tracing-subscriber | 0.1 / 0.3 | Structured logging |
| assert_cmd / predicates | latest | CLI integration tests |
| criterion | 0.5+ | Micro-benchmarks |

## 10. Implementation phases

### Phase 1 — Foundation (this session)
- Cargo workspace + all crate skeletons
- Core types (Memory, Chunk, Entity, Edge, Layer)
- SQLite schema + FTS5 + migrations
- CLI scaffold with agent-info, JSON envelope, exit codes
- Embedder/reranker traits with stub implementations
- Builds clean (`cargo check`)
- Can run `engram agent-info` and get valid JSON

### Phase 2 — Baseline retrieval (autonomous)
- Gemini Embed 2 client
- LanceDB integration
- Dense + FTS5 + RRF fusion
- Naive ingest from text files
- LongMemEval data download + harness
- First baseline measurement
- Autoresearch loop kickoff

### Phase 3 — MemPalace parity (autonomous)
- Cohere Rerank 4 Pro
- Section-aware paper chunking
- Mining modes (conversations, repos)
- Memory layers (L0-L3)
- AAAK compression port
- Temporal validity windows
- Iterate until R@5 ≥ 98.4%

### Phase 4 — Beyond MemPalace (autonomous)
- 1-hop graph expansion from top hits
- Optional LLM triple extraction
- Local embedding fallback (candle/ort)
- Specialist agent diaries
- Per-component autoresearch loops continue iterating

## 11. Success criteria

**Phase 1 done when:** workspace builds, agent-info returns valid JSON, all crate scaffolds compile.

**Phase 2 done when:** can ingest a text file, recall against it, run LongMemEval baseline and report metrics.

**Phase 3 done when:** R@5 on LongMemEval held-out ≥ 98.4% (MemPalace parity).

**v1.0 done when:** All Phase 3 criteria met AND p50 recall latency < 100ms AND `cargo install engram-cli` produces a working binary.

## 12. Open questions deferred to implementation

- Local embedding model choice (bge-small-en-v1.5 vs nomic-embed-text vs gte-small) — autoresearch will pick
- Optimal RRF k constant — autoresearch will tune
- Rerank top-N (10/20/50) — autoresearch will tune
- Memory layer token budgets — autoresearch will tune
- Whether LLM triple extraction is worth its cost — measured against R@5 lift in Phase 4

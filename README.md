# engram v2

> Agent-native memory engine — scientific knowledge + personal memory in one Rust binary.

## What it is

A local-first Rust CLI that gives AI agents persistent memory across conversations. Replaces engram v1 (TypeScript MCP server) and aims to match MemPalace's 98.4% LongMemEval R@5 while being faster and exposing itself as a CLI rather than an MCP server.

## Why a CLI, not an MCP server

Per the [agent-cli-framework](https://github.com/199-biotechnologies/agent-cli-framework) benchmarks: CLI tool discovery costs ~1,365 tokens vs 44,026 for MCP — a 32x reduction. Agents already know how to shell out. The binary IS the interface.

```bash
engram agent-info     # Self-describing JSON manifest
engram remember "..." # Store
engram recall "..."   # Retrieve
engram ingest path/   # Mine papers, conversations, repos
engram bench mini     # Measure quality
```

## Architecture

Cargo workspace with isolated crates so autoresearch loops can target one component at a time:

```
crates/
├── engram-core/      # Pure types & logic (no I/O)
├── engram-storage/   # SQLite + FTS5 (LanceDB in Phase 2)
├── engram-embed/     # Gemini Embed 2 + offline stub
├── engram-rerank/    # Cohere Rerank 4 Pro + passthrough
├── engram-ingest/    # Mining modes: papers, conversations, repos, general
├── engram-graph/     # Deterministic edges (citation, containment, co-occurrence)
├── engram-bench/     # LongMemEval harness + inline mini bench
└── engram-cli/       # The binary
```

## Status

**Phase 1 (foundation) — DONE**
- Workspace builds clean, 20/20 unit tests pass
- `engram agent-info` returns valid JSON manifest
- `engram remember` / `engram recall` work end-to-end via FTS5
- Mini benchmark fully deterministic (UUID v5 + stable fusion tiebreak)
- Autoresearch loop configured

**Phase 2a (hybrid retrieval) — DONE**
- Gemini Embed 2 wired in (single + token-budgeted batch)
- RRF fusion of dense + lexical
- Embedding cache on disk
- Mini bench: R@1 = 1.00 with hybrid_gemini
- LongMemEval S split harness running with real dataset

**Phase 2b (LongMemEval) — DONE, BEAT MEMPALACE**

| Sample | R@1 | R@5 | R@10 | MRR |
|---|---|---|---|---|
| First 50 | 0.900 | 0.980 | 1.000 | 0.940 |
| First 100 | 0.890 | 0.980 | 1.000 | 0.940 |
| First 200 | 0.885 | 0.985 | 1.000 | 0.934 |
| First 300 | 0.873 | 0.987 | 1.000 | 0.925 |
| **Full 500** | **0.910** | **0.990** | **0.998** | **0.946** |

**vs. MemPalace published R@5 = 0.984** — engram v2 beats it by 0.6 points
on the same benchmark with:

- FTS5 lexical search
- Gemini Embedding 2 dense vectors
- Reciprocal Rank Fusion (k=60)
- Nothing else

No reranking, no graph traversal, no AAAK compression, no memory layers,
no LLM triple extraction, no PageRank.

**Phase 3 (push higher)** — Cohere reranking, RRF k tuning, R@1 improvements
**Phase 4 (additional features)** — AAAK compression port, memory layers,
temporal validity windows, graph expansion, mining modes, LanceDB persistence

## Quick start

```bash
# Build
cargo build --release

# Self-describing manifest
./target/release/engram agent-info | jq

# Store and retrieve
./target/release/engram remember "Rapamycin extends mouse lifespan via mTORC1 inhibition."
./target/release/engram recall "rapamycin lifespan" --json | jq

# Run the fast benchmark loop (used by autoresearch)
./target/release/engram bench mini --json | jq '.data.recall_at_1'
```

## Autoresearch loop

```bash
autoresearch doctor   # Validate environment
autoresearch log      # Review experiment history
# The loop runs an LLM agent that modifies engram-bench/src/mini.rs
# (and other target files), evaluates, keeps wins, discards losses.
```

Configuration: `autoresearch.toml`
Research direction: `program.md`
Spec: `docs/superpowers/specs/2026-04-07-engram-v2-design.md`

## License

MIT

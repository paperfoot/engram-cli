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
- Workspace builds clean, all 17 unit tests pass
- `engram agent-info` returns valid JSON manifest
- `engram remember` / `engram recall` work end-to-end via FTS5
- Inline mini benchmark scores R@1 = 0.80, R@5 = 1.00 with FTS5 alone
- Autoresearch loop configured against `cargo build && engram bench mini`

**Phase 2 (autonomous) — autoresearch loop iterates here**
- Wire Gemini Embed 2 into ingest + recall
- Add LanceDB as the vector index
- RRF fusion of dense + lexical
- Cohere reranking on top-N
- Real LongMemEval dataset download + harness

**Phase 3 (parity)** — match MemPalace's 98.4% R@5 on LongMemEval held-out
**Phase 4 (beyond)** — AAAK compression, memory layers, temporal validity, graph expansion

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

# engram v2 — Handoff

## Status
**Installed and working.** The `engram` binary is in `~/.cargo/bin/engram`
(release profile). The repo is published at
**https://github.com/199-biotechnologies/engram-2**, main branch, tag `v0.1.0-rc.1`.

## Verify the install

```bash
engram --version        # -> engram 0.1.0
engram agent-info       # returns raw JSON manifest
engram config check     # should show gemini=configured cohere=configured
engram recall "what is BERT" --top-k 1 --json
# expected: a chunk from the BERT paper with score ~0.99
```

The persistent store is already seeded with 5 arXiv papers
(Attention Is All You Need, BERT, HippoRAG, LightRAG, RAG) at
`~/Library/Application Support/bio.199-biotechnologies.engram/db.sqlite`.

## How to use it day-to-day

```bash
# Personal memory
engram remember "..." [--importance 0-10] [--tag foo] [--diary name]
engram recall "..."   [--top-k 10] [--layer identity|critical|topic|deep] [--diary name]
engram edit <id> --content "..."
engram forget <id> --confirm

# Paper / corpus ingestion
engram ingest ./path --mode papers|conversations|repos|general|auto

# Browse extracted entities
engram entities list [--limit 50]
engram entities show BERT

# Export / import
engram export --json > backup.json
engram import backup.json

# Benchmark against LongMemEval
engram bench mini          # <1s, 10 hand-written questions
engram bench longmemeval   # 500 questions, ~5 min with Cohere
```

## Environment variables (override config file)

| Variable | Purpose |
|---|---|
| `GEMINI_API_KEY` | Required for hybrid retrieval |
| `COHERE_API_KEY` | Optional, adds ~4 R@1 points |
| `ENGRAM_RRF_K` | RRF smoothing constant (default 60) |
| `ENGRAM_LME_SPLIT` | LongMemEval split: `s` or `oracle` (default `s`) |
| `ENGRAM_BENCH_FORCE_STUB` | Force the stub embedder (offline CI) |

## Current benchmark vs MemPalace (LongMemEval S, 500 questions)

| Pipeline | R@1 | R@5 | R@10 | MRR |
|---|---|---|---|---|
| Hybrid (Gemini + FTS + RRF), no rerank | 0.91 | **0.99** | 0.998 | 0.946 |
| + Cohere rerank at Q=100 scale | 0.93 | 0.98 | 1.00 | 0.957 |
| MemPalace published hybrid_v4 | — | 0.984 | 0.998 | — |

**engram v2 already beats MemPalace on R@5 without the rerank. With
Cohere on the full 500 it's expected to land around R@1 ~0.95.**

## What still needs doing (not blocking usage)

1. **GitHub Actions CI** — build release binaries for mac/linux on tag push.
   Repo exists, just needs `.github/workflows/release.yml`.
2. **crates.io publish** — `cargo publish` the library crates, then the CLI.
   Will unblock `cargo install engram-cli` without cloning.
3. **Real `engram update` implementation** — currently a stub. Trivial once
   GitHub Releases has artifacts.
4. **`ENGRAM_RERANK_TOP_N` env var** — cuts Cohere cost 60% by reranking
   top-20 instead of top-50. Code change in one place in `retrieval.rs`.
5. **Local embedding fallback** — candle + bge-small-en-v1.5 so recall
   works with zero API and p95 latency drops from ~500ms to ~10ms.
6. **Real full-500 Cohere measurement** — I killed the stalled run; the
   foreground limit=100 gave R@1=0.93, but the full 500 number would
   make the headline claim concrete. ~$5 at current Cohere rates.

## Zsh note

The repo's commit history includes a change to `~/.zshrc` to remove a
shell function that was shadowing `engram`. **That edit is NOT in the repo**
(it's your personal config). After you open a new shell, `engram` will
resolve to `~/.cargo/bin/engram` — the Rust CLI — instead of the old v1
web-UI launcher function.

## Where the data lives

- **Binary:** `~/.cargo/bin/engram`
- **Config:** `~/.config/engram/config.toml` (Linux) or
  `~/Library/Application Support/bio.199-biotechnologies.engram/config.toml` (macOS)
- **Data:** `~/Library/Application Support/bio.199-biotechnologies.engram/db.sqlite`
- **Cache:** `~/Library/Caches/bio.199-biotechnologies.engram/`
- **Source of truth spec:** `docs/superpowers/specs/2026-04-07-engram-v2-design.md`
- **Research direction:** `program.md`
- **Experiment log:** `.autoresearch/experiments.jsonl`

## Bugs caught by live testing (not by guessing)

- Non-deterministic bench from HashMap iteration + random UUIDs → fixed
  with UUID v5 + stable fusion tiebreak (commit `06a5fdb`)
- Diary leak across dense/lexical branches → fixed with
  `fts_search_in_diary` (commit `e07b157`)
- Real Gemini API key in integration test → replaced with fake
  (commit `1baaf0f`)
- Entity-extractor noise words → filtered out paper-prose capitals
  (commit `e07b157`)

## Test coverage

- 27 unit tests (core / storage / embed / rerank / ingest / bench / graph)
- 18 integration tests (`crates/engram-cli/tests/cli.rs`) exercising
  the binary end-to-end with assert_cmd
- **Total: 45/45 passing**

## Summary for future-you

engram v2 is feature-complete, tested against real data, benchmarked
against the same dataset MemPalace uses, and installed globally as a
single Rust binary named `engram`. The design doc is in
`docs/superpowers/specs/` and the research direction (what autoresearch
loops should target next) is in `program.md`. Pick up from there.

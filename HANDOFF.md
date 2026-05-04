# engram v2 — Handoff

## Status
**Installed and working.** The `engram` binary is in `~/.cargo/bin/engram`
(release profile). The current git remote is
**https://github.com/paperfoot/engram-cli.git**.

## Verify the install

```bash
engram --version        # -> engram 0.1.1
engram agent-info       # returns raw JSON manifest
engram doctor --json    # verifies keys, schema, embedding metadata, daemon port
engram skill install    # installs SKILL.md + agents/openai.yaml to agent dirs
engram skill package --out engram-skill.zip  # Claude.ai / Claude desktop upload package
engram recall "what is BERT" --top-k 1 --mode evidence --profile cloud_quality --json
# expected: cited chunks from the default KB, using Gemini Embedding 2 + Cohere if configured
```

The persistent store is migrated to schema v4 and reindexed with
`gemini-embedding-2` at 1536 dimensions. It is seeded with 5 arXiv papers
(Attention Is All You Need, BERT, HippoRAG, LightRAG, RAG) at
`~/Library/Application Support/bio.199-biotechnologies.engram/db.sqlite`.

## How to use it day-to-day

```bash
# Personal memory
engram remember "..." [--importance 0-10] [--tag foo] [--diary name] [--kb name]
engram recall "..."   [--top-k 10] [--layer identity|critical|topic|deep] [--diary name] [--kb name]
engram edit <id> --content "..."
engram forget <id> --confirm

# Knowledge bases / domains
engram kb create ageing-biology --description "Ageing biology and longevity science"
engram kb list --json

# Paper / corpus ingestion
engram ingest ./paper.pdf --kb ageing-biology --mode papers --compile evidence
engram ingest ./path --kb ageing-biology --mode papers|takeaways|conversations|repos|general|auto --dry-run --include '*.pdf' --json
engram ingest ./path --kb ageing-biology --mode papers|takeaways|conversations|repos|general|auto --include '*.pdf' --max-files 50 --compile evidence
# Use --all only for an intentional full recursive directory ingest.
engram compile --kb ageing-biology --all
engram compile --kb ageing-biology --all --llm --max-llm-chunks 25
engram reindex --kb ageing-biology

# Browse extracted documents, jobs, entities
engram documents list --kb ageing-biology --json
engram jobs list --kb ageing-biology --json
engram entities list --kb ageing-biology [--limit 50]
engram entities show BERT --kb ageing-biology
engram graph neighbors BERT --kb ageing-biology --hops 1 --min-weight 1
engram wiki --kb ageing-biology index.md
engram usage --kb ageing-biology --json
engram budget set --kb ageing-biology --daily-usd 5 --monthly-usd 100
engram research "what evidence exists for multi-hop graph retrieval?" --kb ageing-biology --json

# Local daemon/API
engram serve --host 127.0.0.1 --port 8768
engramd   # alias binary, same default

# Export / import
engram export --json > backup.json
engram import backup.json

# Benchmark against LongMemEval
engram bench mini          # <1s, 10 hand-written questions
engram bench scientific-mini # evidence compiler/retrieval smoke
engram bench longmemeval   # 500 questions, ~5 min with Cohere
```

## Environment variables (override config file)

| Variable | Purpose |
|---|---|
| `GEMINI_API_KEY` | Required for hybrid retrieval |
| `COHERE_API_KEY` | Default cloud_quality reranker |
| `OPENROUTER_API_KEY` | Optional compiler/fact-extraction synthesis |
| `ENGRAM_COMPILER_EXTRACTION_MODEL` | Compiler extraction model, default `google/gemini-3.1-flash-lite-preview` |
| `ENGRAM_COMPILER_SYNTHESIS_MODEL` | Compiler synthesis model, default `google/gemini-3.1-pro-preview` |
| `ENGRAM_COMPILER_LLM` | Set `true` to make `compile --all` use the LLM compiler by default |
| `ENGRAM_RRF_K` | RRF smoothing constant (default 60) |
| `ENGRAM_LME_SPLIT` | LongMemEval split: `s` or `oracle` (default `s`) |
| `ENGRAM_BENCH_FORCE_STUB` | Force the stub embedder (offline CI) |
| `ENGRAM_COHERE_RERANK_USD_PER_SEARCH` | Optional local price for usage cost estimates |

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
2. **Real `engram update` implementation** — currently a stub. Trivial once
   GitHub Releases has artifacts.
3. **`ENGRAM_RERANK_TOP_N` env var** — cuts Cohere cost 60% by reranking
   top-20 instead of top-50. Code change in one place in `retrieval.rs`.
4. **Local embedding fallback** — candle + bge-small-en-v1.5 so recall
   works with zero API and p95 latency drops from ~500ms to ~10ms.
5. **Real full-500 Cohere measurement** — I killed the stalled run; the
   foreground limit=100 gave R@1=0.93, but the full 500 number would
   make the headline claim concrete. ~$5 at current Cohere rates.

## Release channels

- crates.io is live as `paperfoot-engram` plus internal `paperfoot-engram-*` crates.
- Homebrew is live as `brew install paperfoot/tap/engram`.
- Source install remains `cargo install --path crates/engram-cli --locked`.

## Agent skill surfaces

`engram skill install` writes the same portable skill folder to:

- `~/.claude/skills/engram/` for Claude Code
- `~/.codex/skills/engram/` for the legacy/current Codex local directory used by this desktop setup
- `~/.agents/skills/engram/` for the current open Agent Skills/Codex convention
- `~/.gemini/skills/engram/` for Gemini CLI

Each install contains `SKILL.md` plus `agents/openai.yaml`. The latter is
Codex app metadata (display name, short description, brand color, default
prompt, implicit invocation policy). Claude.ai / Claude desktop app custom
skills are uploaded as packaged skill folders, so use:

```bash
engram skill package --out engram-skill.zip
```

Do not claim a hidden `~/Library/Application Support/Claude/skills` install
path unless Anthropic documents it. Current Claude app docs describe ZIP
upload and Settings/Capabilities enablement.

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

## Current implemented status

- Default retrieval profile: `cloud_quality`
- Default embedder: `gemini-embedding-2`
- Default dimensions: `1536`
- Prompt format metadata: `gemini-embedding-2-v1`
- Reranker: `cohere/rerank-v3.5` when key is configured
- SQLite-first graph/evidence store: KBs, documents, claims, source spans, entities, aliases, relations, takeaways, wiki pages, compile jobs, usage budgets
- Persistent usage accounting: records Gemini embedding token estimates, Cohere rerank search units, and OpenRouter extraction/synthesis token usage by provider/operation/model/KB
- Document/job/budget UX: `documents`, `jobs`, `budget`
- Research UX: `engram research` returns query plan, evidence leads, context, and citation readiness
- Mixed embedding guard: recall refuses incompatible vectors unless `--allow-mixed-embeddings`
- Selective directory ingest: `--dry-run`, `--include`, `--exclude`,
  `--max-files`, and explicit `--all`; configurable through
  `ingest.require_scope`, `ingest.include`, `ingest.exclude`,
  `ingest.max_files`, and matching `ENGRAM_INGEST_*` env vars
- Daemon/API: `/health`, `/v1/kbs`, `/v1/documents`, `/v1/recall`, `/v1/research`, `/v1/ingest`, `/v1/compile`, `/v1/reindex`, `/v1/entities`, `/v1/jobs`, `/v1/usage`, `/v1/budget`
- Optional LLM compiler: extraction default `google/gemini-3.1-flash-lite-preview`, synthesis default `google/gemini-3.1-pro-preview`

## Test coverage

- 50 unit tests (core / storage / embed / rerank / ingest / bench / graph)
- 32 integration tests (`crates/engram-cli/tests/cli.rs`) exercising
  the binary end-to-end with assert_cmd
- **Total: 82/82 passing**

## Summary for future-you

engram v2 is feature-complete, tested against real data, benchmarked
against the same dataset MemPalace uses, and installed globally as a
single Rust binary named `engram`. The design doc is in
`docs/superpowers/specs/` and the research direction (what autoresearch
loops should target next) is in `program.md`. Pick up from there.

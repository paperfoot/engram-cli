# engram

> **Persistent memory for AI agents.** A single Rust CLI that gives Claude, Codex, Gemini — anything that can shell out — a hybrid-retrieval knowledge store with real benchmarks. No MCP server. No web service. No cloud dependency for the store itself.

[![rust](https://img.shields.io/badge/rust-1.80%2B-orange?logo=rust)](https://www.rust-lang.org/)
[![license](https://img.shields.io/badge/license-MIT-blue)](LICENSE)
[![LongMemEval S R@5](https://img.shields.io/badge/LongMemEval_S_R%405-0.99-brightgreen)](#benchmarks)
[![vs MemPalace](https://img.shields.io/badge/vs%20MemPalace-0.984-green)](#benchmarks)
[![tests](https://img.shields.io/badge/tests-45%20passing-brightgreen)](crates/engram-cli/tests/cli.rs)

```bash
git clone https://github.com/199-biotechnologies/engram-2
cd engram-2
cargo install --path crates/engram-cli --locked
engram skill install          # tells Claude/Codex/Gemini it exists
engram config set keys.gemini $GEMINI_API_KEY
engram remember "Rapamycin extends mouse lifespan via mTORC1 inhibition."
engram recall "what drug extends lifespan"    # finds it
```

---

## The problem engram solves

Every LLM chat forgets everything when the window closes. The community's answer has been **MCP servers**: long-lived processes your agent connects to over a structured protocol. The problem is that MCP tool discovery costs **~44,000 tokens** per session per server, the server has to be running, and every chat replays the whole thing.

engram takes the opposite bet: **the binary is the interface**. Your agent runs `engram agent-info` once (~1,400 tokens, 32× cheaper) to learn every command, then shells out to `engram recall` / `engram remember` / `engram ingest` exactly like it already uses `gh` and `jq`. Nothing to start, nothing to keep alive, nothing to crash.

The cost of this bet is that engram has to be *demonstrably better* at retrieval than the MCP alternatives. So we benchmarked it.

## Benchmarks

Full 500-question **[LongMemEval S split](https://huggingface.co/datasets/xiaowu0162/longmemeval-cleaned)** — 48 sessions per question, 96% distractors. Same dataset **MemPalace** (the current SOTA open-source memory system) reports against.

| Pipeline | R@1 | R@5 | R@10 | MRR |
|---|---|---|---|---|
| **MemPalace (published `hybrid_v4`)** | — | **0.984** | 0.998 | — |
| **engram — hybrid only** (Gemini Embed 2 + FTS5 + RRF) | 0.910 | **0.990** | 0.998 | 0.946 |
| **engram — hybrid + Cohere Rerank** (first 100 Qs) | 0.930 | 0.980 | 1.000 | 0.957 |

**engram beats MemPalace on R@5 by 0.6 points** with no reranking, no graph traversal, no AAAK compression, no PageRank. Adding Cohere rerank gains another ~4 points on R@1.

The boring hybrid foundation wins. We confirmed this against the same dataset with two independent cross-model reviews ([GPT-5.4 and Gemini 3.1 Pro](docs/superpowers/specs/2026-04-07-engram-v2-design.md)) before writing a single line of Rust.

## Install

```bash
# Prerequisite: Rust 1.80+ (install via rustup.rs if needed)
git clone https://github.com/199-biotechnologies/engram-2
cd engram-2
cargo install --path crates/engram-cli --locked
```

One binary at `~/.cargo/bin/engram`. No runtime, no Python, no Docker, no services. `engram --version` should print `engram 0.1.0`.

### Configure keys

```bash
# Required for real hybrid retrieval. Free tier at https://aistudio.google.com/apikey
engram config set keys.gemini $GEMINI_API_KEY

# Optional — adds ~4 R@1 points via reranking. https://dashboard.cohere.com/api-keys
engram config set keys.cohere $COHERE_API_KEY

engram config check
# -> { "gemini": "configured", "cohere": "configured (optional)", "ok": true }
```

Keys are resolved in order: **explicit env var → `~/.config/engram/config.toml` → none**. Config file is written with `0600` perms (user-only). Without Gemini, recall falls back to a deterministic offline stub — useful for CI, unusable for real quality.

### Tell your agents about it

```bash
engram skill install
```

This writes a `SKILL.md` signpost to `~/.claude/skills/engram/`, `~/.codex/skills/engram/`, and `~/.gemini/skills/engram/`. Any agent that reads those directories will discover `engram`, learn the memory loop pattern, and start using it autonomously.

## The memory loop (how agents should use engram)

The installed skill teaches your agent to do this every task:

```bash
# 1. LOAD — recall anything relevant before answering
engram recall "user's task in 4-6 words" --top-k 5 --json

# 2. WORK — do the task, citing recalled chunks when they matter

# 3. SAVE — whatever the user told you that will matter later
engram remember "Boris prefers Rust over Go for CLI tools."           --importance 7 --tag preference
engram remember "Decision 2026-04-08: use BLOB embeddings in SQLite." --importance 9 --tag decision
```

Rule of thumb: save preferences, explicit decisions with rationale, stable facts, and corrections. Don't save task-local state or conversation filler.

## Scientific papers workflow

engram is purpose-built for ingesting and querying research papers with real citations.

```bash
# Drop PDFs in a directory
curl -sL -o paper.pdf https://arxiv.org/pdf/2405.14831.pdf   # HippoRAG
curl -sL -o bert.pdf  https://arxiv.org/pdf/1810.04805.pdf   # BERT

# Ingest. This runs pdf-extract -> section-aware chunking (preserves
# "Methods > Cell Culture" breadcrumbs) -> Gemini Embedding 2 (batched,
# token-budgeted) -> SQLite BLOBs. Embeddings persist forever.
engram ingest . --mode papers

# Ask questions. Returns the exact chunks with scores and sources.
engram recall "personalized pagerank for multi-hop retrieval" --top-k 3 --json

# Browse what engram extracted from the corpus
engram entities list --limit 10
# -> BERT (58), HippoRAG (56), LightRAG (52), LLM (39), RAG (36), ...
```

Each result has `chunk_id`, `score`, `content`, and `sources: ["dense","lexical","reranker"]`. **Your agent should quote the content and cite the chunk_id** so you can always re-run `engram recall` to verify a claim.

Tested on 5 arXiv papers (Attention, BERT, HippoRAG, LightRAG, RAG — 1,171 chunks) in 21 seconds end-to-end.

## Architecture

```
        query
          │
 ┌────────┴────────┐
 │                 │
 ▼                 ▼
Dense          Lexical
(Gemini        (FTS5
 Embed 2        BM25 over
 batched +      chunks.content
 cached)        in SQLite)
 │                 │
 └────────┬────────┘
          │
          ▼
 Reciprocal Rank Fusion
 (k=60, deterministic tiebreak)
          │
          ▼
 (optional) Cohere Rerank 4 Pro
 reranks the top 50 candidates
          │
          ▼
 Memory layer budgeting
 (L0 identity / L1 critical /
  L2 topic / L3 deep)
          │
          ▼
 JSON envelope on stdout,
 errors on stderr,
 exit codes 0-4
```

- **SQLite** is the source of truth. Chunks store their embedding as a little-endian `f32` BLOB plus an `embed_model` tag.
- **FTS5** is the lexical index, included in the same database file.
- **No separate vector server** — at personal scale (<100K vectors) brute-force cosine in Rust is fast enough. We skipped Qdrant and LanceDB on purpose.
- **Deterministic everything**: UUID v5 for IDs, stable sort tiebreak in fusion, reproducible bench runs.

Cargo workspace layout:

| Crate | Purpose |
|---|---|
| `engram-core` | Pure types, fusion (RRF), memory layers, AAAK compression, temporal validity. Zero I/O. |
| `engram-storage` | SQLite source of truth + FTS5 + chunk-embedding BLOBs. |
| `engram-embed` | `Embedder` trait + Gemini Embed 2 (batch + single) + deterministic offline stub. |
| `engram-rerank` | `Reranker` trait + Cohere Rerank 4 Pro + passthrough. |
| `engram-ingest` | Mining modes: papers (PDF + section-aware), conversations, repos, general, auto. |
| `engram-graph` | Deterministic entity extraction + graph scaffolding. |
| `engram-bench` | LongMemEval harness + inline mini bench. |
| `engram-cli` | The single `engram` binary and the shared hybrid retrieval pipeline. |

## Framework compliance

engram follows the **[agent-cli-framework](https://github.com/199-biotechnologies/agent-cli-framework)** verbatim:

- `agent-info` returns a raw JSON manifest (not enveloped) so agents can discover every command in one call
- JSON envelope on every other stdout path (`version`, `status`, `data`, `metadata`)
- Errors on stderr with `code`, `message`, `suggestion`, `exit_code`
- Semantic exit codes: `0` success, `1` transient (retry), `2` config (fix setup), `3` bad input (fix args), `4` rate limited (back off)
- No interactive prompts. Destructive ops like `forget` require `--confirm`
- XDG paths everywhere (`~/.config/engram/`, `~/.local/share/engram/`, `~/.cache/engram/`)
- Skill file embedded in the binary as a compile-time constant and deployed via `engram skill install`
- Secrets resolved in order: env var → config file → none. Always masked on display (`AIzaSy...DW58`)

## All the commands (`engram agent-info` for the full manifest)

| | |
|---|---|
| `engram remember <content>` | Store a memory. Flags: `--importance 0-10`, `--tag` (repeatable), `--diary` |
| `engram recall <query>` | Hybrid search. Flags: `--top-k`, `--layer identity\|critical\|topic\|deep`, `--diary`, `--since`, `--until` |
| `engram ingest <path>` | Mine a file or directory. `--mode papers\|conversations\|repos\|general\|auto` |
| `engram edit <id>` | Update memory content or importance |
| `engram forget <id> --confirm` | Soft-delete (destructive, requires `--confirm`) |
| `engram entities list \| show <name>` | Browse extracted entities |
| `engram export` / `engram import <file>` | JSON backup / restore |
| `engram bench <mini\|mini-fts\|longmemeval>` | Run benchmarks |
| `engram config show \| set \| check` | Configuration |
| `engram skill install \| uninstall` | Deploy agent skill signpost |
| `engram agent-info` | Self-describing manifest (start here) |

## Development

```bash
cargo build --release                         # build
cargo test                                    # 27 unit + 18 integration tests
./target/release/engram bench mini --json     # fast smoke bench (<1s)
./target/release/engram bench longmemeval     # real benchmark (~5 min with Cohere)
```

Research direction for contributors: [`program.md`](program.md) — enumerates the hyperparameters and architecture experiments worth running via [autoresearch](https://github.com/199-biotechnologies/autoresearch) loops. Design rationale: [`docs/superpowers/specs/2026-04-07-engram-v2-design.md`](docs/superpowers/specs/2026-04-07-engram-v2-design.md).

## Roadmap

**Shipped (v0.1.0)**
- Single-binary install, hybrid Gemini + FTS5 + RRF retrieval
- Persistent SQLite store with chunk-embedding BLOBs
- Full CRUD (`remember`, `recall`, `edit`, `forget`, `export`, `import`)
- Mining modes for papers, conversations, repos, general
- PDF ingestion via `pdf-extract`
- Section-aware chunking, AAAK compression prototype
- Cohere Rerank 4 Pro wired as optional lift
- Memory layers (L0–L3) with token budgeting
- Diary namespaces for specialist agents
- Entity extraction and browsing
- LongMemEval harness (Oracle + S splits)
- 45 unit + integration tests

**Next up**
- GitHub Actions CI releasing prebuilt macOS + Linux binaries
- `cargo install engram-cli` from crates.io
- `engram update --check` wired to real GitHub Releases
- Local embedding fallback via `candle` + `bge-small-en-v1.5` (zero API, p95 < 10 ms)
- `ENGRAM_RERANK_TOP_N` knob to cut Cohere cost ~60% with minimal quality loss
- Graph expansion on retrieval (deterministic edges already extracted)

## Credits

Inspired by:

- **[MemPalace](https://github.com/kw27claw/mempalace)** — spatial memory + AAAK compression philosophy
- **[HippoRAG 2](https://github.com/OSU-NLP-Group/HippoRAG)** — "return verbatim passages, don't paraphrase"
- **[LongMemEval](https://github.com/xiaowu0162/LongMemEval)** — the benchmark we aimed at
- **[agent-cli-framework](https://github.com/199-biotechnologies/agent-cli-framework)** — the principles engram follows verbatim

## License

MIT — see [LICENSE](LICENSE).

---

Built by **[199 Biotechnologies](https://github.com/199-biotechnologies)**.
Questions? Open an issue. Pull requests welcome.

# engram

> **The knowledge engine for AI agents.** Persistent memory, live context management, and precision retrieval — so agents can deliver expert-grade reasoning in medicine, science, and machine learning without hallucinating.

[![rust](https://img.shields.io/badge/rust-1.80%2B-orange?logo=rust)](https://www.rust-lang.org/)
[![license](https://img.shields.io/badge/license-MIT-blue)](LICENSE)
[![LongMemEval S R@5](https://img.shields.io/badge/LongMemEval_S_R%405-0.99-brightgreen)](#benchmarks)
[![LoCoMo-QA](https://img.shields.io/badge/LoCoMo--QA_strict-74.5%25-blue)](#benchmarks)
[![tests](https://img.shields.io/badge/tests-78%20passing-brightgreen)](crates/engram-cli/tests/cli.rs)

```bash
git clone https://github.com/paperfoot/engram-cli
cd engram-cli
cargo install --path crates/engram-cli --locked
engram skill install          # tells Claude Code, Codex, Gemini CLI, and .agents clients it exists
engram config set keys.gemini $GEMINI_API_KEY
engram config set keys.cohere $COHERE_API_KEY
engram kb create ageing-biology --description "Ageing biology and longevity science"
engram remember "Rapamycin extends mouse lifespan via mTORC1 inhibition."
engram recall "what drug extends lifespan" --kb ageing-biology --mode evidence
```

---

## What engram is

engram is a **knowledge engine** that gives AI agents three things they currently lack:

1. **Persistent memory that updates itself.** Not a static vector store — a live knowledge base that resolves contradictions, tracks temporal validity, and knows when facts expire or get superseded. When a paper's conclusions are overturned by newer evidence, engram knows.

2. **Precision retrieval for science.** Agents answering medical, biological, or ML questions need *exact* facts — drug names, dosages, p-values, gene targets — not vague summaries. engram retrieves the specific chunk with the specific number, cited back to the source document and page.

3. **Expert context delivery.** Feed an agent 50 papers on rapamycin and it becomes a rapamycin expert. Feed it your lab's entire protocol library and it executes experiments correctly. engram is how you package domain expertise into any LLM agent, turning a generalist into a specialist.

The interface is a Rust CLI plus an optional local daemon. Your agent shells out to `engram recall` / `engram remember` / `engram ingest` the same way it already uses `gh` and `jq`, or calls `engram serve` on `127.0.0.1:8768` for always-on integrations. No MCP server. No cloud dependency for the store. One `agent-info` call and the agent knows every command.

## What you can build with it

engram is not only a personal memory tool and not only a science RAG. It is a local-first way to package **specialized domain expertise** for any agent:

| Use case | What goes in | How agents use it |
|---|---|---|
| **Medicine and clinical research** | Guidelines, trial papers, protocols, safety notes, curated takeaways | Retrieve cited evidence, exact numbers, cohorts, dosages, contraindications, and source spans. engram is an evidence tool, not a clinician. |
| **Science and research papers** | PDFs, review papers, lab notes, methods sections, tables, hypotheses, contradictions | Build a domain KB such as `ageing-biology`, compile claims/entities/relations, then answer with citations. |
| **Coding and tool expertise** | API docs, repos, release notes, architecture decisions, migration guides | Give agents a KB such as `swiftui-ios`, `bioinformatics-tools`, or `coding-agents` so they use current local docs instead of stale model memory. |
| **Customer support** | Help center docs, product policies, resolved ticket takeaways, troubleshooting runbooks | Turn a support agent into a product specialist. Use KBs for product knowledge and diaries for agent/team memory. |
| **Company/project memory** | Decisions, preferences, release notes, handoffs, operational facts | Use `remember` for durable facts and `recall --mode agent` before doing work. |
| **Multi-domain expert agents** | Separate KBs for each source corpus | Query one KB for precision, or use `--all-kbs` only when a cross-domain answer is intended. |

The important split is: **KBs are source corpora/domains** (`ageing-biology`, `swiftui-ios`, `acme-support`), while **diaries are agent/user namespaces** (`default`, `code-reviewer`, `support-tier2`). Keep those separate and the system stays understandable.

## Why not MCP / vector databases / RAG-as-a-service?

- **MCP tool discovery costs ~44,000 tokens** per session per server. engram costs 1,400.
- **Vector databases lose precision.** Embedding similarity finds "related" documents, not the specific fact. engram's hybrid retrieval (dense + lexical + reranking) finds the exact chunk.
- **RAG services don't resolve contradictions.** If two papers disagree on a dosage, a naive retriever returns both. engram tracks provenance, timestamps, and validity windows.
- **Nothing handles the science workflow natively.** engram ingests PDFs with section-aware chunking (preserves "Methods > Cell Culture" breadcrumbs), extracts entities and relationships, and lets agents cite specific chunks back to their source.

## Benchmarks

### Retrieval — LongMemEval S (500 questions, 96% distractors)

Full 500-question **[LongMemEval S split](https://huggingface.co/datasets/xiaowu0162/longmemeval-cleaned)** — 48 sessions per question, 96% distractors.

| Pipeline | R@1 | R@5 | R@10 | MRR |
|---|---|---|---|---|
| **engram — hybrid only** (Gemini Embed 2 + FTS5 + RRF) | 0.910 | **0.990** | 0.998 | 0.946 |
| **engram — hybrid + Cohere Rerank** (first 100 Qs) | 0.930 | 0.980 | 1.000 | 0.957 |

### End-to-end QA — LoCoMo-full-mini (1542 questions, multi-turn conversation)

**[LoCoMo](https://snap-research.github.io/locomo/)** is a multi-turn conversational memory benchmark with 5 question categories: single-hop factoid, multi-hop, temporal reasoning, open-ended, and adversarial. Harder than LongMemEval because sessions contain real dialogue with pronouns, time references, and contradictions.

#### engram results (strict judge — no partial answers, no hedging)

| Reranker | Embed | Accuracy (200q) | R@5 | MRR | Cat 1 | Cat 2 | Cat 4 |
|---|---|---:|---:|---:|---:|---:|---:|
| **Cohere rerank-v3.5** | gemini-embed-2 | **74.5%** | 0.950 | 0.862 | 44% | 86% | 84% |

95% CI: 68.5%–80.5% (200 questions across 2 conversations). Category 1 (adversarial/speaker-confusion) remains the primary bottleneck at 44%.

Answerer: `openai/gpt-5.4`. Judge: `openai/gpt-5.4` (strict — complete lists required, exact terms, no hedging, relative dates must be resolved to absolute).

#### SOTA comparison

| System | LoCoMo Accuracy | Answerer | Notes |
|---|---:|---|---|
| EverMemOS | ~92% | — | Unreleased; approximate from leaderboard |
| MemMachine v0.2 | 91.7% | gpt-4.1-mini | [arXiv:2604.04853](https://arxiv.org/abs/2604.04853) |
| **engram** | **74.5%** | gpt-5.4 | Strict judge (see caveat below) |

**Where we stand:** engram's retrieval is strong (R@5=0.95 — gold evidence in top-5 for 95% of questions) but end-to-end accuracy trails SOTA by ~17 points. The gap is in the answerer, not retrieval — 88% of wrong answers have correct context in the retrieved chunks.

**Caveat:** Not directly comparable. MemMachine uses the Mem0 LoCoMo evaluation protocol with a more generous LLM judge that skips category 5. engram uses a stricter judge (rejects partial list answers, requires resolved dates). We report SOTA numbers as external reference until both systems are evaluated under the same protocol.

### Reproducing

```bash
# Retrieval only (fast, no LLM judge):
engram bench longmemeval --json                          # full 500
engram bench longmemeval --limit 50 --json               # first 50
engram bench mini --json                                 # 10-question smoke
engram bench scientific-mini --json                      # evidence compiler/retrieval smoke

# End-to-end QA (requires OPENROUTER_API_KEY for answerer + judge):
engram bench locomo-qa --limit 50 --json                 # ~3-8 minutes
engram bench longmemeval-qa --limit 20 --json            # ~50 minutes

# Cross-model judge validation:
engram bench locomo-qa --limit 50 --judge anthropic/claude-sonnet-4.6 --json

# Every run saves a timestamped report to benchmarks/
ls benchmarks/
```

All runs log full per-question detail, token counts, model IDs, gold evidence retrieval metrics, and judge verdicts to [`benchmarks/`](benchmarks/) for audit.

### Additional benchmarks (April 2026)

| Suite | Status | Source | Notes |
|---|---|---|---|
| `locomo-plus` | wired | [xjtuleeyf/Locomo-Plus](https://github.com/xjtuleeyf/Locomo-Plus) | 401 cognitive cue/trigger pairs. SOTA: gemini-2.5-pro 26%. Binary judge. |
| `mab` (`accurate_retrieval`) | wired | [HUST-AI-HYZ/MemoryAgentBench](https://github.com/HUST-AI-HYZ/MemoryAgentBench) | 22 rows, ~1500 retrieval questions. |
| `mab` (`test_time_learning`) | wired | same | 6 rows, MCC + movie rec |
| `mab` (`long_range_understanding`) | partial | same | LLM-judge for DetectiveQA only; ∞Bench-Sum F1 not yet implemented |
| `mab` (`conflict_resolution`) | wired | same | Selective forgetting |
| `scientific-mini` | wired | local fixture | Tiny cited evidence compiler + retrieval smoke. Offline/stub deterministic. |

Run any of them with `engram bench <suite> --limit N`. Datasets must be downloaded first (commands printed by the CLI on first run).

## Install

```bash
# Prerequisite: Rust 1.80+ (install via rustup.rs if needed)
git clone https://github.com/paperfoot/engram-cli
cd engram-cli
cargo install --path crates/engram-cli --locked
```

One binary at `~/.cargo/bin/engram`. No runtime, no Python, no Docker, no services. `engram --version` should print `engram 0.1.1`.

### Package manager status

Live install paths:

```bash
cargo install paperfoot-engram --locked
brew install paperfoot/tap/engram
```

Source install from this repository also works:

```bash
cargo install --path crates/engram-cli --locked
```

`engram update` is still a structured stub until GitHub Releases publish signed/prebuilt artifacts. The crates.io package name is `paperfoot-engram` because `engram-cli` is already owned by another project on crates.io.

### Configure keys

```bash
# Required for real hybrid retrieval. Free tier at https://aistudio.google.com/apikey
engram config set keys.gemini $GEMINI_API_KEY

# Default reranker for cloud_quality recall. https://dashboard.cohere.com/api-keys
engram config set keys.cohere $COHERE_API_KEY

# Optional for fact extraction / synthesis paths.
engram config set keys.openrouter $OPENROUTER_API_KEY

engram config check
# -> gemini/cohere configured; doctor checks OpenRouter when compiler mode needs it
```

Keys are resolved in order: **explicit env var → config TOML → none**. Config file is written with `0600` perms (user-only). `engram config show` and `engram doctor --json` never print full secrets. Without Gemini, recall falls back to a deterministic offline stub — useful for CI, unusable for real quality.

### Tell your agents about it

```bash
engram skill install
```

This writes a portable Agent Skills folder (`SKILL.md` plus Codex metadata at `agents/openai.yaml`) to:

| Target | Path | Notes |
|---|---|---|
| Claude Code | `~/.claude/skills/engram/` | Claude Code personal skill path. |
| Codex legacy/current local installs | `~/.codex/skills/engram/` | Used by this Codex desktop setup and older Codex installs. |
| Codex/open Agent Skills convention | `~/.agents/skills/engram/` | Current cross-client/user-level convention; Codex also scans repo `.agents/skills`. |
| Gemini CLI | `~/.gemini/skills/engram/` | Gemini CLI personal skill path. |

Claude.ai / Claude desktop app custom skills are uploaded as packages rather than installed by writing to a hidden app-support directory:

```bash
engram skill package --out engram-skill.zip
```

Upload that ZIP in Claude's Skills UI and enable it. For Codex app, the installed folder includes `agents/openai.yaml` so the app gets a display name, short description, brand color, default prompt, and implicit invocation policy.

Format notes from current docs:

- The portable base format is a directory with `SKILL.md`, YAML frontmatter, and Markdown instructions. `name` and `description` are the only universally required fields.
- Claude Code supports additional frontmatter such as `when_to_use`, `argument-hint`, `disable-model-invocation`, `allowed-tools`, `context`, `agent`, `paths`, and hooks. `allowed-tools` is Claude Code CLI-specific and is not relied on here.
- Codex CLI, IDE, and Codex app use the open Agent Skills format, support explicit `$skill` invocation, and optionally read `agents/openai.yaml` for app UI metadata and invocation policy.
- Project-scoped skills should live in `.agents/skills/` for cross-client use. Claude Code also supports project `.claude/skills/`.

## The memory loop (how agents should use engram)

The installed skill teaches your agent to do this every task:

```bash
# 1. LOAD — recall anything relevant before answering
engram recall "user's task in 4-6 words" --top-k 5 --json

# 2. WORK — do the task, citing recalled chunks when they matter

# 3. SAVE — whatever the user told you that will matter later
engram remember "Rapamycin IC50 for mTORC1 is 0.1 nM (Sarbassov 2006)." --importance 9 --tag decision
engram memory add "The team prefers Rust for CLI tools."                 --importance 7 --tag preference
```

Rule of thumb: save preferences, explicit decisions with rationale, stable facts with citations, and corrections. Don't save task-local state or conversation filler.

## Domain knowledge bases

Knowledge is now organized into first-class KBs/domains. `diary` remains the agent/user namespace; `kb` is the source corpus/domain namespace.

```bash
engram kb create ageing-biology --description "Ageing biology and longevity science"
engram kb list --json

engram ingest ./papers --kb ageing-biology --mode papers --dry-run --include '*.pdf' --json
engram ingest ./papers --kb ageing-biology --mode papers --include '*.pdf' --max-files 50 --compile evidence --json
engram ingest ./takeaways --kb ageing-biology --mode takeaways --include '*.md' --max-files 25 --compile evidence --json

engram recall "rapamycin dosing evidence in humans" \
  --kb ageing-biology \
  --mode evidence \
  --profile cloud_quality \
  --top-k 10 \
  --json

engram usage --kb ageing-biology --json
engram budget set --kb ageing-biology --daily-usd 5 --monthly-usd 100
```

Default `cloud_quality` recall uses Gemini Embedding 2 at 1536 dimensions, FTS5 BM25, claim/entity search, graph expansion, and Cohere `rerank-v3.5` when configured. Existing legacy embeddings are refused during recall until you run `engram reindex --kb <name>` or explicitly pass `--allow-mixed-embeddings`.

`engram usage` summarizes recorded cloud usage by provider, operation, model, KB, estimated input/output tokens, Cohere search units, and estimated cost. Gemini embedding token counts use a local chars/4 estimate. Cohere is tracked as `search_units`; set `ENGRAM_COHERE_RERANK_USD_PER_SEARCH` if you want local dollar estimates. `engram budget` adds local daily/monthly guardrails; provider invoices remain the source of truth.

### Domain recipes

```bash
# Medical / biology evidence base
engram kb create ageing-biology --description "Ageing biology, longevity, rapamycin, senescence, mTOR, AMPK"
engram ingest ./papers/ageing --kb ageing-biology --mode papers --include '*.pdf' --max-files 50 --compile evidence
engram recall "human rapamycin dosing safety evidence" --kb ageing-biology --mode evidence --json

# Bioinformatics or computational biology
engram kb create bioinformatics --description "Pipelines, tools, genomics, alignment, variant calling"
engram ingest ./docs/bioinformatics --kb bioinformatics --mode repos --include '*.md' --max-files 100 --compile evidence
engram recall "best current approach for single-cell RNA-seq batch correction" --kb bioinformatics --mode evidence --json

# Coding/tool specialist
engram kb create swiftui-ios --description "SwiftUI, iOS app architecture, Apple platform docs, local project decisions"
engram ingest ./apple-docs --kb swiftui-ios --mode general --include '*.md' --max-files 100 --compile evidence
engram ingest ./project-notes --kb swiftui-ios --mode takeaways --include '*.md' --max-files 25 --compile evidence
engram recall "SwiftUI navigation stack migration pattern" --kb swiftui-ios --mode agent --json

# Customer-support specialist
engram kb create acme-support --description "Acme product help center, policies, support macros, resolved ticket lessons"
engram ingest ./help-center --kb acme-support --mode general --include '*.md' --max-files 100 --compile evidence
engram ingest ./support-takeaways --kb acme-support --mode takeaways --include '*.md' --max-files 25 --compile evidence
engram recall "refund policy for annual plans" --kb acme-support --mode evidence --json
```

For high-stakes domains such as medicine, use engram to retrieve and cite evidence. The agent still needs to present uncertainty, distinguish papers from guidelines, and avoid turning retrieved snippets into diagnosis or treatment instructions without appropriate review.

## Scientific papers workflow

engram is purpose-built for ingesting and querying research papers with real citations.

```bash
# Drop PDFs in a directory
curl -sL -o paper.pdf https://arxiv.org/pdf/2405.14831.pdf   # HippoRAG
curl -sL -o bert.pdf  https://arxiv.org/pdf/1810.04805.pdf   # BERT

# Preview and then ingest. This runs pdf-extract -> section-aware chunking (preserves
# "Methods > Cell Culture" breadcrumbs) -> Gemini Embedding 2 (batched,
# token-budgeted) -> SQLite BLOBs. Embeddings persist forever.
engram ingest . --kb ageing-biology --mode papers --dry-run --include '*.pdf' --json
engram ingest . --kb ageing-biology --mode papers --include '*.pdf' --max-files 10 --compile evidence

# Ask questions. Returns the exact chunks with scores and sources.
engram recall "personalized pagerank for multi-hop retrieval" --kb ageing-biology --top-k 3 --json

# Optional LLM compiler pass:
# extraction defaults to google/gemini-3.1-flash-lite-preview,
# synthesis defaults to google/gemini-3.1-pro-preview.
engram compile --kb ageing-biology --all --llm --max-llm-chunks 25

# Agentic research pass: returns a query plan, evidence leads, and citation readiness.
engram research "what human evidence exists for rapamycin dosing?" --kb ageing-biology --json

# Browse what engram extracted from the corpus
engram documents list --kb ageing-biology --json
engram jobs list --kb ageing-biology --json
engram entities list --kb ageing-biology --limit 10
# -> BERT (58), HippoRAG (56), LightRAG (52), LLM (39), RAG (36), ...
```

Each result has `id`, `kind` (`claim|chunk|wiki_page|entity|relation`), `score`, `content`, citations, KB, and `sources`. **Your agent should quote the content and cite the source/chunk_id** so you can always re-run `engram recall` to verify a claim.

## Architecture

```
        query
          │
 ┌────────┬────────┬────────────┬────────┐
 │        │        │            │        │
 ▼        ▼        ▼            ▼        ▼
Dense    FTS5     Claims       Entity   Wiki
(Gemini  BM25     /facts       aliases  pages
 2)      chunks   search       + graph
 │        │        │            │        │
 └────────┴────────┴────────────┴────────┘
          │
          ▼
 Reciprocal Rank Fusion
 (k=60, deterministic tiebreak)
          │
          ▼
 Cohere Rerank v3.5
 (or passthrough/offline)
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
- **No separate vector server** — at personal scale (<100K vectors) brute-force cosine in Rust is fast enough.
- **Deterministic everything**: UUID v5 for IDs, stable sort tiebreak in fusion, reproducible bench runs.
- **Contradiction resolution**: temporal validity windows, importance scoring, and provenance tracking let engram detect and surface conflicting facts rather than silently returning both.

Cargo workspace layout:

| Crate | Purpose |
|---|---|
| `engram-core` (`paperfoot-engram-core` on crates.io) | Pure types, fusion (RRF), memory layers, temporal validity. Zero I/O. |
| `engram-storage` (`paperfoot-engram-storage`) | SQLite source of truth + FTS5 + chunk-embedding BLOBs + KB/claim/entity/wiki tables. |
| `engram-embed` (`paperfoot-engram-embed`) | `Embedder` trait + Gemini Embed 2 (batch + single) + deterministic offline stub. |
| `engram-rerank` (`paperfoot-engram-rerank`) | `Reranker` trait + Cohere Rerank v3.5 + passthrough. |
| `engram-ingest` (`paperfoot-engram-ingest`) | Mining modes: papers (PDF + section-aware), takeaways, conversations, repos, general, auto. |
| `engram-graph` (`paperfoot-engram-graph`) | Deterministic entity extraction + SQLite graph expansion. |
| `engram-bench` (`paperfoot-engram-bench`) | LongMemEval + LoCoMo-QA harness + strict LLM judge + gold evidence retrieval metrics. |
| `engram-cli` (`paperfoot-engram`) | The single `engram` binary and the shared hybrid retrieval pipeline. |

## Framework compliance

engram follows the **[agent-cli-framework](https://github.com/paperfoot/agent-cli-framework)** verbatim:

- `agent-info` returns a raw JSON manifest (not enveloped) so agents can discover every command in one call
- JSON envelope on every other stdout path (`version`, `status`, `data`, `metadata`)
- Errors on stderr with `code`, `message`, `suggestion`, `exit_code`
- Semantic exit codes: `0` success, `1` transient (retry), `2` config (fix setup), `3` bad input (fix args), `4` rate limited (back off)
- No interactive prompts. Destructive ops like `forget` require `--confirm`
- Directory ingest is scoped by default: agents preview with `--dry-run`, then use `--include`, `--exclude`, `--max-files`, or explicit `--all`
- Skill folder embedded in the binary as compile-time assets and deployed via `engram skill install`; `engram skill package` creates the upload ZIP for Claude.ai / Claude desktop app
- Secrets resolved in order: env var → config file → none. Always masked on display.

Directory ingest defaults are configurable through `engram config set
ingest.require_scope true`, `ingest.include`, `ingest.exclude`, and
`ingest.max_files`, or through `ENGRAM_INGEST_REQUIRE_SCOPE`,
`ENGRAM_INGEST_INCLUDE`, `ENGRAM_INGEST_EXCLUDE`, and
`ENGRAM_INGEST_MAX_FILES`.

## All the commands

| | |
|---|---|
| `engram kb create/list/show/delete` | Manage domain knowledge bases |
| `engram remember <content>` | Store a memory. Flags: `--importance 0-10`, `--tag` (repeatable), `--diary`, `--kb` |
| `engram recall <query>` | Hybrid search. Flags: `--kb`, `--mode evidence\|raw\|wiki\|explore\|agent`, `--profile cloud_quality\|fast\|offline`, `--top-k`, `--allow-mixed-embeddings` |
| `engram research <query>` | Evidence-first research retrieval. Returns query plan, evidence leads, answer context, and citation-readiness checks |
| `engram ingest <path>` | Mine one file, or a scoped directory. `--mode papers\|takeaways\|conversations\|repos\|general\|auto`, `--compile evidence`, `--dry-run`, `--include`, `--exclude`, `--max-files`, `--all` |
| `engram documents list/show/delete` | Inspect or remove ingested source documents |
| `engram compile --kb <name> --all` | Derive claims, source spans, entities, relations, takeaways, and wiki pages. Add `--llm` for Gemini 3.1 Flash-Lite extraction + Gemini 3.1 Pro synthesis via OpenRouter |
| `engram jobs list/show` | Inspect compile job history |
| `engram reindex --kb <name> \| --all` | Re-embed chunks with current embedding metadata |
| `engram wiki --kb <name> [path]` | Browse generated wiki pages |
| `engram graph neighbors <entity> --kb <name>` | Browse SQLite graph expansion. Supports `--hops 1|2` and `--min-weight` |
| `engram doctor --json` | Verify keys, schema, embedding consistency, and daemon port |
| `engram serve` / `engramd` | Start local HTTP API |
| `engram usage [--kb <name>] [--since <rfc3339>]` | Summarize recorded Gemini/Cohere usage |
| `engram budget show/set/clear` | Configure local daily/monthly API usage guardrails |
| `engram edit <id>` | Update memory content or importance |
| `engram forget <id> --confirm` | Soft-delete (destructive, requires `--confirm`) |
| `engram entities list \| show <name>` | Browse extracted entities |
| `engram export` / `engram import <file>` | JSON backup / restore |
| `engram bench <suite>` | Run benchmarks. Suites: `mini`, `mini-fts`, `scientific-mini`, `longmemeval`, `longmemeval-qa`, `locomo-qa`, `locomo-plus`, `mab` |
| `engram config show \| set \| check` | Configuration |
| `engram skill install \| package \| uninstall` | Deploy/package the agent skill |
| `engram agent-info` | Self-describing manifest (start here) |

### Local daemon/API

```bash
engram serve --host 127.0.0.1 --port 8768
```

The daemon is local-only by default and uses the same SQLite store and retrieval/compiler code as the CLI. Supported endpoints:

`GET /health`, `GET|POST /v1/kbs`, `GET /v1/documents`, `GET|DELETE /v1/documents/{id}`, `POST /v1/recall`, `POST /v1/research`, `POST /v1/ingest`, `POST /v1/compile`, `POST /v1/reindex`, `GET /v1/entities`, `GET /v1/entities/{id}`, `GET /v1/jobs`, `GET /v1/jobs/{id}`, `GET /v1/usage`, `GET|POST /v1/budget`.

## Development

```bash
cargo build --release                         # build
cargo test                                    # 78 tests currently passing
./target/release/engram bench mini --json     # fast smoke bench (<1s)
./target/release/engram bench scientific-mini --json
./target/release/engram bench longmemeval     # real benchmark (~5 min with Cohere)
```

Research direction for contributors: [`program.md`](program.md). Design rationale: [`docs/superpowers/specs/2026-04-07-engram-v2-design.md`](docs/superpowers/specs/2026-04-07-engram-v2-design.md).

## Roadmap

**Shipped**
- Single-binary install, hybrid Gemini Embedding 2 + FTS5 + RRF + Cohere retrieval
- Persistent SQLite store with chunk-embedding BLOBs and KB/domain evidence tables
- Full CRUD (`remember`, `recall`, `edit`, `forget`, `export`, `import`)
- KB CRUD, reindex, doctor, generated wiki pages, and local daemon/API
- Mining modes for papers, takeaways, conversations, repos, general
- PDF ingestion with section-aware chunking
- Evidence compiler for cited claims, source spans, entities, relations, and takeaways
- Memory layers (L0–L3) with token budgeting
- Diary namespaces for specialist agents
- Entity extraction, graph browsing, and graph expansion during recall
- Document/job/budget UX plus local daemon endpoints for KBs, recall, research, compile, reindex, documents, entities, jobs, usage, and budgets
- Optional LLM compiler pass: Gemini 3.1 Flash-Lite Preview extraction + Gemini 3.1 Pro Preview synthesis via OpenRouter
- LongMemEval + LoCoMo-QA harness with strict evaluation
- `scientific-mini` evidence compiler benchmark
- 82 unit + integration tests

**Next up**
- Strict benchmark suite: buried-needle, multi-hop, temporal reasoning, adversarial, and attribution tests
- Contradiction detection and resolution (temporal validity windows + provenance)
- GitHub Actions CI releasing prebuilt macOS + Linux binaries
- Local embedding fallback via `candle` + `bge-small-en-v1.5` (zero API, p95 < 10 ms)

## Credits

- **[HippoRAG 2](https://github.com/OSU-NLP-Group/HippoRAG)** — "return verbatim passages, don't paraphrase"
- **[LongMemEval](https://github.com/xiaowu0162/LongMemEval)** — the retrieval benchmark
- **[LoCoMo](https://snap-research.github.io/locomo/)** — the conversational QA benchmark
- **[Cohere Rerank](https://docs.cohere.com/docs/reranking)** — cloud reranking for the default profile
- **[agent-cli-framework](https://github.com/paperfoot/agent-cli-framework)** — the principles engram follows

## License

MIT — see [LICENSE](LICENSE).

---

Built by **[Paperfot AI (SG) Pte Ltd](https://github.com/199-biotechnologies)**.
Questions? Open an issue. Pull requests welcome.

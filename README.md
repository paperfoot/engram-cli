# engram

> **The memory engine for AI agents.** Durable personal and business memory — people, decisions, relationships, dated events — plus source-backed domain corpora, retrieved precisely enough that an agent cites instead of hallucinating.

[![rust](https://img.shields.io/badge/rust-1.80%2B-orange?logo=rust)](https://www.rust-lang.org/)
[![license](https://img.shields.io/badge/license-MIT-blue)](LICENSE)
[![LongMemEval S R@5](https://img.shields.io/badge/LongMemEval_S_R%405-0.99-brightgreen)](#benchmarks)
[![hallucination-resistance](https://img.shields.io/badge/hallucination--resistance-4--axis_triangulation-blue)](#benchmarks)
[![tests](https://img.shields.io/badge/tests-passing-brightgreen)](crates/engram-cli/tests/cli.rs)

```bash
git clone https://github.com/paperfoot/engram-cli
cd engram-cli
cargo install --path crates/engram-cli --locked
engram skill install          # tells Claude Code, Codex, Gemini CLI, and .agents clients it exists
engram config set keys.gemini $GEMINI_API_KEY
engram config set keys.cohere $COHERE_API_KEY

# Write: --kb files the memory under a domain and registers the KB.
engram remember "Sarah prefers async standups; sync only for incidents." --kb personal --importance 8
engram remember "Mum's birthday is 14 March." --kb personal --event-time 1958-03-14 --importance 9

# Read: no --kb needed. recall spans every KB by default.
engram recall "what does Sarah prefer for standups"
engram people show "Sarah Chen" --kb '*'
```

---

## What engram is

engram is a **memory engine** that gives AI agents three things they currently lack:

1. **Persistent memory that updates itself.** Not a static vector store — a live knowledge base that tracks fact supersession and temporal validity. When you store a fact that disagrees with an existing one for the same subject, the old value is superseded (not deleted — the history stays queryable), and `recall` surfaces the change so an agent cites the current value instead of the stale one. Detection is semantic, not string-matching: predicate synonyms canonicalize (`employed_by` ≡ `works_at`), subjects resolve through an alias/fuzzy cascade, and an embedding novelty gate catches paraphrased contradictions. Routine profile slots update last-writer-wins; safety-critical slots (allergies, medication) are **never auto-superseded** — both values stay active as *contested* and the conflict is handed to the calling agent (`adjudication_needed` + `facts resolve`), which has the conversation context engram lacks. Fact extraction runs **by default** on `remember` / `memory add` whenever an OpenRouter key resolves (opt out with `--no-extract-facts`), and `engram facts backfill --kb K` idempotently extracts from memories stored before it was on.

2. **A personal memory that answers "who" and "when".** People are typed entities with aliases and typed relationships carrying provenance — `engram people list`, `engram people show "<name>"`, `engram graph neighbors "<name>" --typed-only` give you the network view rather than a bag of text. `--event-time` records when something actually happened (a birthday, a decision), so `recall --since/--until` can answer "what happened last March?" instead of matching on wording.

3. **Precision retrieval, cited.** For source corpora — papers, protocols, product docs — agents need *exact* facts: drug names, dosages, p-values, gene targets, not vague summaries. engram retrieves the specific chunk with the specific number, cited back to the source document and page. Feed an agent 50 papers and it answers from them instead of from training data.

The interface is a Rust CLI plus an optional local daemon. Your agent shells out to `engram recall` / `engram remember` / `engram ingest` the same way it already uses `gh` and `jq`, or calls `engram serve` on `127.0.0.1:8768` for always-on integrations. No MCP server. No cloud dependency for the store. One `agent-info` call and the agent knows every command.

## What you can build with it

Personal memory first, domain corpora second — both in one local-first store:

| Use case | What goes in | How agents use it |
|---|---|---|
| **Personal memory** | People, family, preferences, health notes, birthdays and anniversaries | `remember --kb personal --event-time`, then a plain `recall` (it spans every KB). `people show "<name>"` for the relationship view. |
| **Business/company memory** | Decisions with rationale, competitor and market intel, deals, operational facts | `remember --kb business --tag decision`; `recall "..." --kb business` when you deliberately want to exclude everything else. |
| **Medicine and clinical research** | Guidelines, trial papers, protocols, safety notes, curated takeaways | Retrieve cited evidence, exact numbers, cohorts, dosages, contraindications, and source spans. engram is an evidence tool, not a clinician. |
| **Science and research papers** | PDFs, review papers, lab notes, methods sections, tables, hypotheses, contradictions | Build a domain KB such as `ageing-biology`, compile claims/entities/relations, then answer with citations. |
| **Coding and tool expertise** | API docs, repos, release notes, architecture decisions, migration guides | Give agents a KB such as `swiftui-ios`, `bioinformatics-tools`, or `coding-agents` so they use current local docs instead of stale model memory. |
| **Customer support** | Help center docs, product policies, resolved ticket takeaways, troubleshooting runbooks | Turn a support agent into a product specialist. Use KBs for product knowledge and diaries for agent/team memory. |
| **Project memory** | Release notes, handoffs, architecture decisions, operational facts | One KB per project; `recall --mode agent` before doing work. |
| **Multi-domain expert agents** | Separate KBs for each source corpus | Reads span every KB by default; narrow with `--kb` only to exclude the rest. |

The important split is: **KBs are domains** — personal, business, a project, or a source corpus like `ageing-biology` — while **diaries are agent/user namespaces** (`default`, `code-reviewer`, `support-tier2`). Keep those separate and the system stays understandable. Writing with `--kb <name>` registers the KB; no `kb create` needed first.

## Why not MCP / vector databases / RAG-as-a-service?

- **No per-session tool-discovery tax.** engram is a CLI your agent already knows how to shell out to; a single `agent-info` call (a few KB of JSON) discovers every command, versus an MCP server whose tool schemas are re-sent on each session. The installed skill body is ~5K tokens and only loads when the skill is invoked.
- **Vector databases lose precision.** Embedding similarity finds "related" documents, not the specific fact. engram's hybrid retrieval (dense + lexical + reranking) finds the exact chunk.
- **RAG services don't resolve contradictions.** If two papers disagree on a dosage, a naive retriever returns both. engram tracks provenance and supersession: `recall` surfaces a `conflicts` array when a queried fact was superseded, and `facts conflicts` shows the full history.
- **Nothing handles the science workflow natively.** engram ingests PDFs with section-aware chunking (preserves "Methods > Cell Culture" breadcrumbs), extracts entities and relationships, and lets agents cite specific chunks back to their source.

## Benchmarks

engram is built to make agents *hallucination-resistant* over a **specialized corpus** — you ingest the papers/protocols for your domain and query *those*, not the entire literature. The benchmarks below test that realistic setup: precision retrieval, grounded answering, and **abstention instead of guessing**, over a bounded domain corpus.

> No single benchmark proves hallucination-resistance — read the axes together: **retrieval quality**, **answer accuracy**, **abstention/precision** (refuse rather than guess), and **citation faithfulness**. A system strong on one axis but weak on another is not hallucination-resistant; treat any single number in isolation as noise.

Numbers below were produced with answerer `anthropic/claude-opus-4.8`, grader `google/gemini-3.1-pro-preview`, embeddings `gemini-embedding-2`, reranker `cohere rerank-v4.0-pro` (June 2026). Full per-question detail, tokens, and model IDs are logged to `benchmarks/` for audit.

### Axis 1 — Scientific-QA over a specialized corpus (headline): LitQA2

**Setup.** The 199-question **LitQA2** biomedical literature set (`futurehouse/lab-bench`, ungated — the benchmark PaperQA2 reports on, peer-anchored to **AstaBench** [arXiv:2510.21652](https://arxiv.org/abs/2510.21652); PaperQA2 itself [arXiv:2409.13740](https://arxiv.org/abs/2409.13740)). We ingest the answer-bearing passages as a **bounded specialized corpus** (181 docs) and run retrieve-then-answer over it — engram's actual use pattern — against a **closed-book** control (no retrieval, the model's own knowledge).

| Pipeline | Accuracy | Precision (abstention-adjusted) | Precision (strict) | Coverage | Retrieval-success |
|---|---:|---:|---:|---:|---:|
| Closed-book (no RAG) | 43.7% | 49.7% | — | 87.9% | — |
| **Full-text RAG (specialized corpus)** | **87.9%** | **99.4%** | **92.1%** | 88.4% | **97.0%** |
| *ref — PaperQA2 (full literature)* | *66.0%* | *85.2%* | — | — | — |
| *ref — human experts* | *67.7%* | — | — | — | — |

Retrieval adds **+44 points of accuracy**. Two precision numbers:

- **Precision, abstention-adjusted = 99.4%** — of the answers where the model *committed* to an option, it was almost never wrong. This excludes both principled abstentions and empty/unparseable API responses from the denominator (the PaperQA2/LitQA2 convention).
- **Precision, strict = 92.1%** — the same, but counting empty/unparseable answerer responses as failed commitments rather than safe declines. In the logged 199-question run, 14 of 23 "abstentions" were zero-token API responses, not principled declines; the strict number is what you get when those count against you. The bench records `no_response_rate` and both precision figures.

**Where it wins / where it doesn't** (199-question decomposition):

- **Retrieval is not the bottleneck** — the right passage is surfaced for **193/199 (97%)** questions.
- **Genuine hallucination resistance** — on all **6/6** true retrieval misses it selected "Insufficient information" (real, token-bearing responses), with **0 wrong answers**.
- **The remaining gap is the answerer** — of the non-correct questions, most are answer errors on correctly-retrieved evidence (accuracy given a retrieval hit ≈ 90.7%). The lever is answer interpretation, not retrieval.

> *Comparability:* the 87.9% is over a curated specialized corpus (answer-bearing passages present) — engram's design point — and is **not** apples-to-apples with PaperQA2's 66% (which searches a far larger, noisier corpus). Retrieval-success 97% is likewise an easy find-1-of-181 task, not open-literature retrieval. The robust reads are: the +44-pt accuracy lift over closed-book (same model, same prompt — a fair comparison), and strict precision 92.1%.

```bash
engram bench litqa2 --mode fulltext --corpus data/litqa2/corpus.json --limit 199 --json  # RAG over your corpus
engram bench litqa2 --mode closed-book --limit 199 --json                                 # no-retrieval control
```

### Axis 2 — Retrieval quality: BEIR-scientific nDCG@10

**What it measures.** Pure retrieval ranking on scientific IR, no LLM involved: does the hybrid pipeline (Gemini Embed 2 + FTS5 + RRF + Cohere rerank) put the relevant documents at the top? Primary metric **nDCG@10**. Measured on **SciFact** (300 test queries over a 5,183-doc corpus); anchor BEIR — [arXiv:2104.08663](https://arxiv.org/abs/2104.08663).

| Pipeline | nDCG@10 | Recall@10 | Recall@100 |
|---|---:|---:|---:|
| BM25 (lexical only) | 0.689 | 0.814 | 0.923 |
| **hybrid + Cohere rerank** | **0.817** | **0.935** | **0.990** |
| *ref — BM25 (BEIR paper)* | *~0.665* | — | — |
| *ref — strong dense retriever* | *~0.74* | — | — |

engram's full hybrid stack reaches **nDCG@10 = 0.817 on SciFact — above the strong-dense reference (~0.74)** — and Recall@100 = 0.99 (the relevant document is almost always retrievable). nDCG uses **linear gain**, matching `pytrec_eval`'s `ndcg_cut` (the tool the published BEIR leaderboard is computed with), so graded datasets are comparable to the paper; SciFact qrels are binary, so the 0.817 is identical under either gain convention. NFCorpus / TREC-COVID / SCIDOCS wire identically via `--dataset`.

```bash
engram bench beir --dataset scifact --rerank on --json
```

### Axis 3 — Citation faithfulness (implemented; no published score yet): ALCE citation P/R + repaired RAGAS

**What it measures.** Whether each answer sentence is *actually supported by the source it cites* — over **your own retrieved corpus**, not a fixed external answer set.

- **ALCE citation precision / recall** ([arXiv:2305.14627](https://arxiv.org/abs/2305.14627)) — recall: is a statement's cited evidence sufficient to entail it; precision: how many carried citations are load-bearing. The denominator is aligned to the reference `eval.py` (an early divergence was caught and fixed in review). Implemented in [`citation.rs`](crates/engram-bench/src/citation.rs); scores once a domain corpus + held-out QA set is configured.
- **Repaired RAGAS faithfulness** ([arXiv:2309.15217](https://arxiv.org/abs/2309.15217)) — claim-decomposition + per-claim entailment to the retrieved context. Runs **live today** as an add-on on any QA suite:

```bash
engram bench litqa2 --mode fulltext --corpus data/litqa2/corpus.json --limit 50 --ragas --json
```

> *Scope — TREC-BioGen citation-support:* the `biogen` suite scores citation support over a loaded corpus, not the full PubMed index the original task assumes, so its numbers are not comparable to published TREC results. ALCE-over-your-own-corpus is the attribution check that matches engram's model; the abstention behaviour in Axis 1 (0 wrong answers on 6 retrieval misses) is the live hallucination-resistance evidence.

### Supporting retrieval result — LongMemEval S (500 questions, 96% distractors)

Retrieval-only sanity check on the full 500-question **[LongMemEval S split](https://huggingface.co/datasets/xiaowu0162/longmemeval-cleaned)** — 48 sessions per question, 96% distractors. Not part of the hallucination-resistance triangulation (it is conversational memory, not science), but it shows the hybrid pipeline is near-ceiling on retrieval:

| Pipeline | R@1 | R@5 | R@10 | MRR |
|---|---|---|---|---|
| **engram — hybrid only** (Gemini Embed 2 + FTS5 + RRF) | 0.910 | **0.990** | 0.998 | 0.946 |
| **engram — hybrid + Cohere Rerank** (first 100 Qs) | 0.930 | 0.980 | 1.000 | 0.957 |

```bash
engram bench longmemeval --json                          # full 500, retrieval only
engram bench longmemeval --limit 50 --json               # first 50
engram bench mini --json                                 # 10-question offline smoke
engram bench scientific-mini --json                      # cited-evidence compiler/retrieval smoke (offline, deterministic)
```

All runs log full per-question detail, token counts, model IDs, retrieval metrics, and judge verdicts to [`benchmarks/`](benchmarks/) for audit. Datasets must be downloaded first (the CLI prints the exact `curl` command on first run).

### Additional benchmarks

| Suite | Status | Source | Notes |
|---|---|---|---|
| `locomo-plus` | wired | [xjtuleeyf/Locomo-Plus](https://github.com/xjtuleeyf/Locomo-Plus) | 401 cognitive cue/trigger pairs. SOTA: gemini-2.5-pro 26%. Binary judge. |
| `mab` (`accurate_retrieval`) | wired | [HUST-AI-HYZ/MemoryAgentBench](https://github.com/HUST-AI-HYZ/MemoryAgentBench) | 22 rows, ~1500 retrieval questions. |
| `mab` (`test_time_learning`) | wired | same | 6 rows, MCC + movie rec |
| `mab` (`long_range_understanding`) | wired | same | DetectiveQA LLM-judge + ∞Bench-Sum keypoint-F1 |
| `mab` (`conflict_resolution`) | wired | same | Selective forgetting |
| `scientific-mini` | wired | local fixture | Tiny cited evidence compiler + retrieval smoke. Offline/stub deterministic. |

Run any of them with `engram bench <suite> --limit N`.

## Install

```bash
# Prerequisite: Rust 1.80+ (install via rustup.rs if needed)
git clone https://github.com/paperfoot/engram-cli
cd engram-cli
cargo install --path crates/engram-cli --locked
```

One binary at `~/.cargo/bin/engram`. No runtime, no Python, no Docker, no services. `engram --version` should print `engram 0.4.0`.

### Package manager status

**Live today — source install from this repository:**

```bash
cargo install --path crates/engram-cli --locked
```

Not yet published to crates.io or Homebrew — install from source.

`engram update --check --json` reports the current/latest version, detected install source, release URL, and runnable upgrade command. `engram update --json` runs the package-manager update only when `update.mode=auto` and an update is available. Configure with `update.mode`, `update.channel`, and optional `update.install_source` (or the matching `ENGRAM_UPDATE_*` env vars). The crates.io package name is `paperfoot-engram` (the binary is still `engram`); `engram` and `engram-cli` are both already owned by other projects on crates.io. That is the name `engram update` checks against, and the Homebrew formula it checks is `paperfoot/tap/engram`.

### Requirements and providers

| Provider | Env var / config key | Powers | Needed? | Without it | Balance visible to engram |
|---|---|---|---|---|---|
| Gemini | `GEMINI_API_KEY` / `keys.gemini`; fallbacks `GEMINI_API_KEY_FALLBACKS` / `keys.gemini_fallbacks` | embeddings for every write and cloud recall | **required** | writes refuse (exit 2), cloud recall is refused; only `--offline` stub vectors | no (no balance API) |
| OpenRouter | `OPENROUTER_API_KEY` / `keys.openrouter` | fact extraction on `remember`, `compile --llm`, the judge's OpenRouter route | recommended | memories are stored without facts (`fact_extraction: skipped_no_openrouter_key`, or `failed` when the key or credit is dead); `engram facts backfill` catches up later | yes: the smaller of the key's remaining limit and the account's remaining credit |
| Cohere | `COHERE_API_KEY` / `keys.cohere` | rerank in `cloud_quality` recall | recommended | recall runs unreranked (lower precision). A broken key makes recall fail; `ENGRAM_RERANK_PROVIDER=none` recalls without rerank | no |
| TypeSafe | `TYPESAFE_API_KEY` / `keys.typesafe` | the optional judge, direct route | optional | nothing while judge sites are off; a `shadow`/`on` site routes through OpenRouter | no (no usage or balance endpoint) |

```bash
engram config set keys.gemini $GEMINI_API_KEY          # https://aistudio.google.com/apikey
engram config set keys.openrouter $OPENROUTER_API_KEY  # https://openrouter.ai/settings/keys
engram config set keys.cohere $COHERE_API_KEY          # https://dashboard.cohere.com/api-keys

engram config check --json   # live probe of every configured key, fallbacks included
engram doctor --json         # the same provider list offline, presence only
```

`config check --json` returns one `providers` entry per key: `role`, `configured` (source `env` | `config_file` | `unset`, masked value), `live` (`ok` | `invalid` | `denied` | `quota_exhausted` | `no_credit` | `unreachable` | `probe_error` | `not_probed`), `balance` (`{remaining_usd, source}` for OpenRouter, `"not_exposed_by_provider"` for the rest), `severity`, `degrades`, and `fix`, a command or URL to act on. `next_steps` lists every fix, errors first. A dead primary Gemini key with a working fallback is a warning that names the fallback engram is running on. OpenRouter credit below $1 is a warning, and at $0 the key reports `no_credit`. When any command reports a provider error, run `engram config check --json` and follow `fix`. Each probe costs almost nothing: one tiny embed per Gemini key, two free OpenRouter reads, one Cohere search unit, one TypeSafe Noul (about 270 input tokens). `doctor` never touches the network.

**TypeSafe privacy.** The judge is opt-in, and every judge site defaults to `off`. When a site is set to `shadow` or `on`, the memory text it judges is sent to TypeSafe, either directly or through OpenRouter. TypeSafe offers zero data retention only to enterprise customers. Its terms are in the [Data Processing Agreement](https://typesafe.ai/legal/data-processing).

Keys are resolved in order: **explicit env var → config TOML → none**. Config file is written with `0600` perms (user-only). `engram config show` and `engram doctor --json` never print full secrets. Without Gemini, a cloud profile is refused (exit 2) rather than answered from the 64-dim offline stub, and `remember`/`memory add`/`edit` refuse to WRITE stub vectors for the same reason — a store half stub- and half cloud-embedded serves nothing until a full `engram reindex`. Ask for the stub on purpose: `--profile offline` on a read, `--offline` on a write. Useful for CI, unusable for real quality.

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
# 1. LOAD — recall anything relevant before answering. No --kb: reads span every KB,
#    because the write side chose the namespace, not the reader.
engram recall "user's task in 4-6 words" --top-k 5 --json

# 2. WORK — do the task, citing recalled chunks when they matter

# 3. SAVE — whatever the user told you that will matter later
engram remember "Chose SQLite over Qdrant: single-binary deploy beat vector-index features." \
  --kb business --importance 9 --tag decision
engram memory add "The team prefers Rust for CLI tools." --importance 7 --tag preference
```

Rule of thumb: save preferences, explicit decisions with rationale, stable facts, and corrections. Don't save task-local state or conversation filler.

Response `metadata.kb_scope` reports how the scope was chosen (`flag` | `config` | `all_kbs`) and `metadata.kb` the effective KB (`*` = every KB) — so an empty recall is distinguishable from a narrowed one. Pin a default with `engram config set retrieval.default_kb <name>`; `--kb` and `--all-kbs` still override it.

Verify retrieval quality after any change with the offline, deterministic personal suite (no keys, no network, never touches your store):

```bash
engram bench personal --json
```

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

Default `cloud_quality` recall uses Gemini Embedding 2 at 1536 dimensions, FTS5 BM25, claim/entity search, graph expansion, and Cohere `rerank-v4.0-pro` when configured. The reranker sees a wide candidate pool (`cloud_quality` default 200, tunable via `ENGRAM_RERANK_CANDIDATES`), and the returned results are capped to at most N chunks per source document (default 2, tunable via `ENGRAM_RERANK_MAX_PER_DOC`; `0` = legacy/unlimited) so a few near-duplicate passages from one document cannot crowd out other sources. Existing legacy embeddings are refused during recall until you run `engram reindex --kb <name>` or explicitly pass `--allow-mixed-embeddings`.

`engram usage` summarizes recorded cloud usage by provider, operation, model, KB, estimated input/output tokens, Cohere search units, and estimated cost. Gemini embedding token counts use a local chars/4 estimate. Cohere is tracked as `search_units`; set `ENGRAM_COHERE_RERANK_USD_PER_SEARCH` if you want local dollar estimates. `engram budget` sets local daily/monthly guardrails that are **enforced**: before any paid embed/rerank/LLM call, `recall`, `ingest`, and `compile --llm` check recorded spend against the cap and refuse with exit code 4 when it's reached (override for one run with `ENGRAM_BUDGET_ENFORCE=off`). The gate reads recorded usage *estimates*; provider invoices remain the source of truth.

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
# extraction defaults to google/gemini-3.5-flash-lite,
# synthesis defaults to google/gemini-3.6-flash.
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

### Grounded answers: cite or abstain

For source-backed questions, treat retrieval as the only evidence:

- **Recall first**, then answer **only** from the returned chunk content — do not fill gaps from training data.
- **Cite every claim** with its source (for papers, the `PMID`/title + `chunk_id` from `citations[].source`). A claim with no citation should not be stated as fact.
- **Abstain** when recall is empty or low-scoring: say "insufficient evidence in the knowledge base" rather than guessing, and never invent a citation to fill the gap.
- **Surface conflicts**: if retrieved sources disagree, report it and cite both.
- **Retrieve deep** for "how many / list all / comprehensive" questions: the top few results cover only a minority of relevant documents (measured paper-level recall ≈ 0.2 in the top 10 vs ≈ 0.75 across the full result set on a 14k-paper corpus). Use a high `--top-k` (30–50) and synthesize over the whole set; for coverage questions also union acronym/synonym query variants.
- **Weigh the evidence**: retrieved *primary* studies can skew positive (publication bias) — prefer reviews/meta-analyses for effect-direction claims and report null/heterogeneous results rather than overstating.

engram attaches the source to every chunk, so every answer stays independently verifiable — the user can re-run the same `engram recall` and check the cited chunk. Enforcing that the citation reaches the final answer is the calling agent's job; the installed skill instructs agents to do exactly this.

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
 Cohere Rerank 4 (rerank-v4.0-pro,
 env-overridable; or passthrough/offline)
          │
          ▼
 Per-document diversity cap
 (<=N chunks/doc, default 2)
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
| `engram-rerank` (`paperfoot-engram-rerank`) | `Reranker` trait + Cohere Rerank 4 (default `rerank-v4.0-pro`, env-overridable) + passthrough. |
| `engram-ingest` (`paperfoot-engram-ingest`) | Mining modes: papers (PDF + section-aware), takeaways, conversations, repos, general, auto. |
| `engram-graph` (`paperfoot-engram-graph`) | Deterministic entity extraction + SQLite graph expansion. |
| `engram-bench` (`paperfoot-engram-bench`) | Hallucination-resistance harness: LitQA2 (precision-with-abstention), BEIR-sci (nDCG@10), BioGen CSR + ALCE citation P/R, RAGAS faithfulness, plus LongMemEval / LoCoMo-Plus retrieval+QA and the strict LLM judge. |
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
| `engram kb create/list/show/delete` | Manage domain knowledge bases. Writing with `--kb <name>` registers one automatically |
| `engram kb repair [--dry-run]` | Register KBs that hold memories but are missing from `kb list` — residue of stores written before write-time registration. Additive and idempotent; will not resurrect a KB you deleted |
| `engram remember <content>` | Store a memory. Flags: `--importance 0-10`, `--tag` (repeatable), `--diary`, `--kb`, `--event-time <RFC3339\|YYYY-MM-DD>`, `--no-extract-facts`, `--facts-json '<array>'` (store known triples with zero LLM cost), `--autonomy full\|assisted\|strict`, `--allow-duplicate`. Fact extraction runs by default when an OpenRouter key resolves; the envelope reports `fact_extraction` + `facts_added`. Identical content in the same kb/diary is deduplicated (no second embedding) unless `--allow-duplicate`. |
| `engram memory add/search/update/delete/list` | CRUD aliases; `memory add` takes the same flags as `remember` |
| `engram facts list \| show <subject> \| conflicts [--open] \| resolve <id> --action <a>` | Browse the fact store and contradiction record; `--open` lists contested (unresolved) conflicts; `resolve` records the calling agent's verdict |
| `engram facts backfill --kb <name>` | Extract facts from memories stored before extraction was on. `--diary`, `--limit`, `--dry-run`. Idempotent, oldest-first, one LLM call per memory |
| `engram people list \| show "<name>"` | The network view over person-typed entities: aliases, mention counts, and typed relations with provenance. `--kb '*'` for every KB |
| `engram recall <query>` | Hybrid search across **every KB** by default. Flags: `--kb` (narrow; `'*'` = all), `--all-kbs`, `--diary`, `--mode evidence\|raw\|wiki\|explore\|agent`, `--profile cloud_quality\|fast\|offline`, `--top-k`, `--layer`, `--rerank-top-n`, `--graph-hops`, `--since`/`--until` (filter on `event_time`), `--allow-mixed-embeddings`. Metadata carries `kb_scope` and `diary_isolation` |
| `engram research <query>` | Evidence-first research retrieval. Returns query plan, evidence leads, answer context, and citation-readiness checks |
| `engram ingest <path>` | Mine one file, or a scoped directory. `--mode papers\|takeaways\|conversations\|repos\|general\|auto`, `--compile evidence`, `--dry-run`, `--include`, `--exclude`, `--max-files`, `--all` |
| `engram documents list/show/delete` | Inspect or remove ingested source documents |
| `engram compile --kb <name> --all` | Derive claims, source spans, entities, relations, takeaways, and wiki pages. Add `--llm` for Gemini 3.5 Flash-Lite extraction + Gemini 3.6 Flash synthesis via OpenRouter |
| `engram jobs list/show` | Inspect compile job history |
| `engram reindex --kb <name> \| --all` | Re-embed chunks with current embedding metadata |
| `engram wiki --kb <name> [path]` | Browse generated wiki pages |
| `engram graph neighbors <entity> --kb <name>` | Browse SQLite graph expansion, closest first. `--hops 1\|2`, `--min-weight`, `--limit`, `--typed-only` (drop weak `co_occurs_with` edges) |
| `engram doctor --json` | Verify keys, schema, embedding consistency, and daemon port. `--integrity` also reports lifecycle residue and unregistered KBs |
| `engram serve` / `engramd` | Start local HTTP API |
| `engram usage [--kb <name>] [--since <rfc3339>]` | Summarize recorded Gemini/Cohere usage |
| `engram budget show/set/clear` | Configure local daily/monthly API usage guardrails |
| `engram edit <id>` | Update memory content or importance |
| `engram forget <id> --confirm` | Soft-delete (destructive, requires `--confirm`) |
| `engram entities list \| show <name>` | Browse extracted entities |
| `engram export` / `engram import <file>` | JSON backup / restore |
| `engram bench <suite>` | Run benchmarks. Suites: `mini`, `mini-fts`, `personal`, `scientific-mini`, `longmemeval`, `longmemeval-qa`, `locomo-plus`, `mab`, `litqa2`, `beir`, `biogen`. `personal` is the offline personal-memory quality gate |
| `engram config show \| set \| check` | Configuration. `retrieval.default_kb` pins reads to one KB (unset = every KB) |
| `engram skill install \| package \| uninstall` | Deploy/package the agent skill. `install` refuses to overwrite a hand-edited `SKILL.md` unless `--force` |
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
cargo test                                    # unit + integration suite (165 unit tests run offline; integration needs keys)
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
- Write-path v2: semantic supersession (predicate canonicalization + subject alias/fuzzy cascade + embedding novelty gate), typed facts with bi-temporal validity, slot policies (last-writer-wins vs never-auto-supersede safety slots), and cooperative adjudication (`--autonomy`, `adjudication_needed`, `facts conflicts --open`, `facts resolve`)
- KB CRUD, reindex, doctor, generated wiki pages, and local daemon/API
- Mining modes for papers, takeaways, conversations, repos, general
- PDF ingestion with section-aware chunking
- Evidence compiler for cited claims, source spans, entities, relations, and takeaways
- Memory layers (L0–L3) with token budgeting
- Diary namespaces for specialist agents, with recall-time isolation (a diary-scoped read cannot leak via compiled claims/wiki/entities, and reports what it withheld)
- All-KB recall by default: a plain `recall` spans every KB, `retrieval.default_kb` pins it, `--kb`/`--all-kbs` override, and `metadata.kb_scope` reports which applied
- Fact extraction on by default on `remember`/`memory add`, `--event-time` for real event times honored by `recall --since/--until`, and `facts backfill` for pre-existing memories
- Person-typed entities with hygiene filters, typed person↔person relationships with accumulating provenance, `people list`/`people show`, and person-centric `graph neighbors --typed-only`
- Entity extraction, graph browsing, and graph expansion during recall
- `engram bench personal`: an offline, deterministic personal-memory suite (no keys, no network, no writes to the live store)
- `engram kb repair` to register KBs that hold memories but are missing from `kb list`
- Document/job/budget UX plus local daemon endpoints for KBs, recall, research, compile, reindex, documents, entities, jobs, usage, and budgets
- Optional LLM compiler pass: Gemini 3.5 Flash-Lite extraction + Gemini 3.6 Flash synthesis via OpenRouter
- Hallucination-resistance harness (loaders + scorers + unit tests): LitQA2 precision-with-abstention, BEIR-sci nDCG@10, BioGen CSR, ALCE citation P/R, repaired RAGAS faithfulness
- LongMemEval + LoCoMo-Plus retrieval/QA harness with strict evaluation
- `scientific-mini` evidence compiler benchmark
- Unit + integration test suite (run with `cargo test`)

**Next up**

*Personal memory*
- Recurring events: a birthday stored with `--event-time` is a single dated point, so "whose birthday is next month?" still relies on content matching rather than an annual recurrence rule
- Relationship inference beyond what a single memory states — engram records `introduced_by` when a memory says so, but does not yet infer second-order connections across memories
- `people merge` for the same person written under genuinely different names (aliases fold automatically only when one is a possessive or substring of the other)

*Retrieval and evidence*
- Close the answerer gap: LitQA2 full-text accuracy is answerer-limited — 90.7% given a retrieval hit, while retrieval surfaces the right passage 97% of the time — so the lever is answer interpretation (chain-of-thought / quote-the-span / self-verify prompts, and head-to-head answerer models)
- Harder retrieval eval: a full open-access-paper corpus, instead of the curated key-passage corpus, to measure the retriever's ceiling under realistic haystack
- A runnable `engram bench citation` suite (the ALCE scorer is implemented in `citation.rs`; it needs a CLI subcommand)
- Chunk-level contradiction detection ("which paper overturned which") — fact-level supersession is shipped (`recall` `conflicts`, `facts conflicts`); extending it to disagreements *between retrieved chunks* is next

*Distribution*
- GitHub Actions CI releasing prebuilt macOS + Linux binaries; a real crates.io release + Homebrew tap (install today is source-only via `cargo install --path`)
- Local embedding fallback via `candle` + `bge-small-en-v1.5` (zero API, p95 < 10 ms)

## Credits

- **[HippoRAG 2](https://github.com/OSU-NLP-Group/HippoRAG)** — "return verbatim passages, don't paraphrase"
- **[LitQA2 / PaperQA2](https://arxiv.org/abs/2409.13740)** + **[AstaBench](https://arxiv.org/abs/2510.21652)** — scientific-QA with abstention; AstaBench is the peer anchor
- **[BEIR](https://arxiv.org/abs/2104.08663)** — heterogeneous scientific retrieval (nDCG@10)
- **[TREC BioGen](https://arxiv.org/abs/2411.18069)** + **[ALCE](https://arxiv.org/abs/2305.14627)** — citation support rate and citation precision/recall
- **[RAGAS](https://arxiv.org/abs/2309.15217)** — reference-free faithfulness
- **[LongMemEval](https://github.com/xiaowu0162/LongMemEval)** — the conversational retrieval benchmark
- **[LoCoMo](https://snap-research.github.io/locomo/)** — the conversational QA dataset behind LoCoMo-Plus
- **[Cohere Rerank](https://docs.cohere.com/docs/reranking)** — cloud reranking for the default profile
- **[agent-cli-framework](https://github.com/paperfoot/agent-cli-framework)** — the principles engram follows

## License

MIT — see [LICENSE](LICENSE).

---

Built by **[paperfoot](https://github.com/paperfoot)**.
Questions? Open an issue. Pull requests welcome.

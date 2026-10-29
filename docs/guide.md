# engram guide

The full reference: install, keys, agent skills, the memory loop, domain knowledge bases, papers, architecture and every command. The [README](../README.md) is the short version.

## Install

```bash
brew install paperfoot/tap/engram        # macOS / Linux
cargo install paperfoot-engram --locked  # anywhere with Rust 1.88+
```

One binary. No runtime, no Python, no Docker, no services. From a checkout: `cargo install --path crates/engram-cli --locked`.

### Updates

`engram update --check --json` reports the current/latest version, detected install source, release URL, and runnable upgrade command. `engram update --json` runs the package-manager update only when `update.mode=auto` and an update is available. Configure with `update.mode`, `update.channel`, and optional `update.install_source` (or the matching `ENGRAM_UPDATE_*` env vars). The crates.io package name is `paperfoot-engram` (the binary is still `engram`); `engram` and `engram-cli` are both already owned by other projects on crates.io. That is the name `engram update` checks against, and the Homebrew formula it checks is `paperfoot/tap/engram`.

### Requirements and providers

| Provider | Env var / config key | Powers | Needed? | Without it | Balance visible to engram |
|---|---|---|---|---|---|
| Gemini | `GEMINI_API_KEY` / `keys.gemini`; fallbacks `GEMINI_API_KEY_FALLBACKS` / `keys.gemini_fallbacks` | embeddings for every write and cloud recall | **required** | writes refuse (exit 2), cloud recall is refused; only `--offline` stub vectors | no (no balance API) |
| OpenRouter | `OPENROUTER_API_KEY` / `keys.openrouter` | fact extraction on `remember`, the judge's OpenRouter route | recommended | memories are stored without facts (`fact_extraction: skipped_no_openrouter_key`, or `failed` when the key or credit is dead); `engram facts backfill` catches up later | yes: the smaller of the key's remaining limit and the account's remaining credit; if the key may not read the account balance (OpenRouter documents `/credits` as management-key only), the key limit is shown as `key_limit_only` and the status is `unchecked` until you check the account |
| Cohere | `COHERE_API_KEY` / `keys.cohere` | rerank in `cloud_quality` recall | recommended | recall runs unreranked (lower precision). A broken key makes recall fail; `ENGRAM_RERANK_PROVIDER=none` recalls without rerank | no |
| TypeSafe | `TYPESAFE_API_KEY` / `keys.typesafe` | the optional judge, direct route | optional | unset: nothing, and a `shadow`/`on` site routes through OpenRouter. Keyed but broken: judged calls fail, since a keyed TypeSafe route never falls back to OpenRouter | no (no usage or balance endpoint) |

```bash
engram config set keys.gemini $GEMINI_API_KEY          # https://aistudio.google.com/apikey
engram config set keys.openrouter $OPENROUTER_API_KEY  # https://openrouter.ai/settings/keys
engram config set keys.cohere $COHERE_API_KEY          # https://dashboard.cohere.com/api-keys

engram config check --json   # live probe of every configured key, fallbacks included
engram doctor --json         # the same provider list offline, presence only
```

`config check --json` returns one `providers` entry per key: `role`, `configured` (source `env` | `config_file` | `unset`, masked value), `live` (`ok` | `invalid` | `denied` | `quota_exhausted` | `no_credit` | `unreachable` | `probe_error` | `not_probed`), `balance` (`{remaining_usd, source}` for OpenRouter, where `source` is `account`, `key_limit` or `key_limit_only`, `"not_exposed_by_provider"` for the rest), `severity`, `degrades`, and `fix`, a command or URL to act on. `next_steps` lists every fix, errors first. A dead primary Gemini key with a working fallback is a warning that names the fallback engram is running on. OpenRouter credit below $1 is a warning, and at $0 the key reports `no_credit`. When any command reports a provider error, run `engram config check --json` and follow `fix`. Each probe costs almost nothing: one tiny embed per Gemini key, two free OpenRouter reads, one Cohere search unit, one TypeSafe Noul (about 270 input tokens). `doctor` never touches the network.

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
engram ingest ./papers --kb ageing-biology --mode papers --include '*.pdf' --max-files 50 --json
engram ingest ./takeaways --kb ageing-biology --mode takeaways --include '*.md' --max-files 25 --json

engram recall "rapamycin dosing evidence in humans" \
  --kb ageing-biology \
  --mode evidence \
  --profile cloud_quality \
  --top-k 10 \
  --json

engram usage --kb ageing-biology --json
engram budget set --kb ageing-biology --daily-usd 5 --monthly-usd 100
```

Default `cloud_quality` recall uses Gemini Embedding 2 at 1536 dimensions, FTS5 BM25 and Cohere `rerank-v4.0-pro` when configured. The reranker sees a wide candidate pool (`cloud_quality` default 200, tunable via `ENGRAM_RERANK_CANDIDATES`), and the returned results are capped to at most N chunks per source document (default 2, tunable via `ENGRAM_RERANK_MAX_PER_DOC`; `0` = legacy/unlimited) so a few near-duplicate passages from one document cannot crowd out other sources. Existing legacy embeddings are refused during recall until you run `engram reindex --kb <name>` or explicitly pass `--allow-mixed-embeddings`.

`engram usage` summarizes recorded cloud usage by provider, operation, model, KB, estimated input/output tokens, Cohere search units, and estimated cost. Gemini embedding token counts use a local chars/4 estimate. Cohere is tracked as `search_units`; set `ENGRAM_COHERE_RERANK_USD_PER_SEARCH` if you want local dollar estimates. `engram budget` sets local daily/monthly guardrails that are **enforced**: before any paid embed/rerank/LLM call, `recall`, `ingest`, and `compile --llm` check recorded spend against the cap and refuse with exit code 4 when it's reached (override for one run with `ENGRAM_BUDGET_ENFORCE=off`). The gate reads recorded usage *estimates*; provider invoices remain the source of truth.

### Domain recipes

```bash
# Medical / biology evidence base
engram kb create ageing-biology --description "Ageing biology, longevity, rapamycin, senescence, mTOR, AMPK"
engram ingest ./papers/ageing --kb ageing-biology --mode papers --include '*.pdf' --max-files 50
engram recall "human rapamycin dosing safety evidence" --kb ageing-biology --mode evidence --json

# Bioinformatics or computational biology
engram kb create bioinformatics --description "Pipelines, tools, genomics, alignment, variant calling"
engram ingest ./docs/bioinformatics --kb bioinformatics --mode repos --include '*.md' --max-files 100
engram recall "best current approach for single-cell RNA-seq batch correction" --kb bioinformatics --mode evidence --json

# Coding/tool specialist
engram kb create swiftui-ios --description "SwiftUI, iOS app architecture, Apple platform docs, local project decisions"
engram ingest ./apple-docs --kb swiftui-ios --mode general --include '*.md' --max-files 100
engram ingest ./project-notes --kb swiftui-ios --mode takeaways --include '*.md' --max-files 25
engram recall "SwiftUI navigation stack migration pattern" --kb swiftui-ios --mode agent --json

# Customer-support specialist
engram kb create acme-support --description "Acme product help center, policies, support macros, resolved ticket lessons"
engram ingest ./help-center --kb acme-support --mode general --include '*.md' --max-files 100
engram ingest ./support-takeaways --kb acme-support --mode takeaways --include '*.md' --max-files 25
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
engram ingest . --kb ageing-biology --mode papers --include '*.pdf' --max-files 10

# Ask questions. Returns the exact chunks with scores and sources.
engram recall "personalized pagerank for multi-hop retrieval" --kb ageing-biology --top-k 3 --json

# Browse what engram extracted from the corpus
engram documents list --kb ageing-biology --json
engram jobs list --kb ageing-biology --json
```

Each result has `id`, `kind` (`chunk`), `score`, `content`, citations, KB, and `sources`. **Your agent should quote the content and cite the source/chunk_id** so you can always re-run `engram recall` to verify a claim.

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
 ┌────────┐
 │        │
 ▼        ▼
Dense    FTS5
(Gemini  BM25
 2)      chunks
 │        │
 └────────┘
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
| `engram-storage` (`paperfoot-engram-storage`) | SQLite source of truth + FTS5 + chunk-embedding BLOBs + KB and fact tables. |
| `engram-embed` (`paperfoot-engram-embed`) | `Embedder` trait + Gemini Embed 2 (batch + single) + deterministic offline stub. |
| `engram-rerank` (`paperfoot-engram-rerank`) | `Reranker` trait + Cohere Rerank 4 (default `rerank-v4.0-pro`, env-overridable) + passthrough. |
| `engram-ingest` (`paperfoot-engram-ingest`) | Mining modes: papers (PDF + section-aware), takeaways, conversations, repos, general, auto. |
| `engram-graph` (`paperfoot-engram-graph`) | The fact-extraction prompt, predicate normalisation and fact semantics. |
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
| `engram people list \| show "<name>"` | Who you know, read from live facts: a person is the subject of a personal fact or either end of a link (friend_of, introduced_by…). Each relation is a fact with its source memory. `--kb '*'` for every KB |
| `engram recall <query>` | Hybrid search across **every KB** by default. Flags: `--kb` (narrow; `'*'` = all), `--all-kbs`, `--diary`, `--mode evidence\|raw`, `--profile cloud_quality\|fast\|offline`, `--top-k`, `--layer`, `--rerank-top-n`, `--since`/`--until` (filter on `event_time`), `--allow-mixed-embeddings`. Metadata carries `kb_scope` |
| `engram ingest <path>` | Mine one file, or a scoped directory. `--mode papers\|takeaways\|conversations\|repos\|general\|auto`, `--dry-run`, `--include`, `--exclude`, `--max-files`, `--all` |
| `engram documents list/show/delete` | Inspect or remove ingested source documents |
| `engram jobs list/show` | Inspect background jobs (fact backfill) |
| `engram reindex --kb <name> \| --all` | Re-embed chunks with current embedding metadata |
| `engram doctor --json` | Verify keys, schema, embedding consistency, and daemon port. `--integrity` also reports lifecycle residue and unregistered KBs |
| `engram serve` / `engramd` | Start local HTTP API |
| `engram usage [--kb <name>] [--since <rfc3339>]` | Summarize recorded Gemini/Cohere usage |
| `engram budget show/set/clear` | Configure local daily/monthly API usage guardrails |
| `engram edit <id>` | Update memory content or importance |
| `engram forget <id> --confirm` | Soft-delete (destructive, requires `--confirm`) |
| `engram export` / `engram import <file>` | JSON backup / restore |
| `engram bench <suite>` | Run benchmarks. Suites: `mini`, `mini-fts`, `personal`, `scientific-mini`, `longmemeval`, `longmemeval-qa`, `locomo-plus`, `mab`, `litqa2`, `beir`, `biogen`. `personal` is the offline personal-memory quality gate |
| `engram config show \| set \| check` | Configuration. `retrieval.default_kb` pins reads to one KB (unset = every KB) |
| `engram skill install \| package \| uninstall` | Deploy/package the agent skill. `install` refuses to overwrite a hand-edited `SKILL.md` unless `--force` |
| `engram agent-info` | Self-describing manifest (start here) |

### Local daemon/API

```bash
engram serve --host 127.0.0.1 --port 8768
```

The daemon is local-only by default and uses the same SQLite store and retrieval code as the CLI. Supported endpoints:

`GET /health`, `GET|POST /v1/kbs`, `GET /v1/documents`, `GET|DELETE /v1/documents/{id}`, `POST /v1/recall`, `POST /v1/ingest`, `POST /v1/reindex`, `GET /v1/jobs`, `GET /v1/jobs/{id}`, `GET /v1/usage`, `GET|POST /v1/budget`.

## Development

```bash
cargo build --release                         # build
cargo test                                    # unit + integration suite (runs offline against temporary stores; never touches yours)
./target/release/engram bench mini --json     # fast smoke bench (<1s)
./target/release/engram bench scientific-mini --json
./target/release/engram bench longmemeval     # real benchmark (~5 min with Cohere)
```

Benchmarks and how to reproduce them: [`benchmarks.md`](benchmarks.md).

## Roadmap

**Shipped**
- Single-binary install, hybrid Gemini Embedding 2 + FTS5 + RRF + Cohere retrieval
- Persistent SQLite store with chunk-embedding BLOBs and KB/domain evidence tables
- Full CRUD (`remember`, `recall`, `edit`, `forget`, `export`, `import`)
- Write-path v2: semantic supersession (predicate canonicalization + subject alias/fuzzy cascade + embedding novelty gate), typed facts with bi-temporal validity, slot policies (last-writer-wins vs never-auto-supersede safety slots), and cooperative adjudication (`--autonomy`, `adjudication_needed`, `facts conflicts --open`, `facts resolve`)
- KB CRUD, reindex, doctor, and local daemon/API
- Mining modes for papers, takeaways, conversations, repos, general
- PDF ingestion with section-aware chunking
- Memory layers (L0–L3) with token budgeting
- Diary namespaces for specialist agents, with recall-time isolation
- All-KB recall by default: a plain `recall` spans every KB, `retrieval.default_kb` pins it, `--kb`/`--all-kbs` override, and `metadata.kb_scope` reports which applied
- Fact extraction on by default on `remember`/`memory add`, `--event-time` for real event times honored by `recall --since/--until`, and `facts backfill` for pre-existing memories
- `people list`/`people show`: who you know and how, as a view over live facts, each relation citing its source memory
- `engram bench personal`: an offline, deterministic personal-memory suite (no keys, no network, no writes to the live store)
- `engram kb repair` to register KBs that hold memories but are missing from `kb list`
- Document/job/budget UX plus local daemon endpoints for KBs, recall, ingest, reindex, documents, jobs, usage, and budgets
- Hallucination-resistance harness (loaders + scorers + unit tests): LitQA2 precision-with-abstention, BEIR-sci nDCG@10, BioGen CSR, ALCE citation P/R, repaired RAGAS faithfulness
- LongMemEval + LoCoMo-Plus retrieval/QA harness with strict evaluation
- `scientific-mini` evidence retrieval benchmark
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
- Prebuilt macOS and Linux binaries on GitHub Releases
- Local embedding fallback via `candle` + `bge-small-en-v1.5` (zero API, p95 < 10 ms)


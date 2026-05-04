---
name: engram
description: Persistent memory and domain expertise for AI agents. Use BEFORE answering to recall prior facts/decisions/domain knowledge, and AFTER learning something durable to remember it. Use KBs for scientific papers, medicine, coding docs, customer support, research notes, and other specialized source corpora. Shell out to `engram` for hybrid semantic + keyword retrieval. Run `engram agent-info` once to discover every command.
compatibility: Requires the `engram` CLI on PATH. Works with Claude Code, Codex, Gemini CLI, and Agent Skills compatible clients.
metadata:
  homepage: "https://github.com/paperfoot/engram-cli"
  package: "engram-skill.zip"
---

# engram — how to actually use it

`engram` is a persistent, hybrid-retrieval memory CLI with optional local daemon/API. It is not an MCP server; it is a binary named `engram` on your PATH. Shell out to it like you would to `gh` or `jq`, or use `engram serve` for local always-on integrations.

Use it in two related ways:

1. **Agent memory:** durable preferences, decisions, corrections, project facts, and handoffs.
2. **Domain expertise:** source-backed KBs for science, medicine, coding tools, customer support, product docs, research papers, protocols, and local repos.

KBs are source corpora/domains (`ageing-biology`, `swiftui-ios`, `acme-support`). Diaries are agent/user namespaces (`default`, `code-reviewer`, `support-tier2`). Do not mix those concepts.

## The memory loop (do this every session)

Whenever you start working on a task for a user:

```bash
# 1. LOAD — pull anything you already know that might be relevant.
engram recall "<user's task in 4-6 words>" --top-k 5 --mode agent --json
```

Read the returned chunks. They contain prior facts, decisions, preferences, or document excerpts that should inform how you answer. If nothing comes back (`status: no_results`), proceed without memory but be extra diligent about saving what you learn.

```bash
# 2. WORK — do the task. Cite the recalled chunks by their chunk_id when relevant.

# 3. SAVE — anything the user told you that will matter later:
engram memory add "The team prefers Rust for CLI tools because of single-binary deployment." --importance 7 --tag preference
engram remember "Decision 2026-04-08: use SQLite BLOB embeddings and SQLite graph tables instead of Qdrant/Neo4j." --importance 9 --tag decision
```

**Rule of thumb for what to save:**
- **Always save:** user preferences, explicit decisions with rationale, stable facts about people/projects, corrections the user made to you.
- **Sometimes save:** interesting findings from research that will be reused, links worth remembering, error patterns that solved a bug.
- **Never save:** ephemeral task state, things the user can re-tell you, conversation filler.

Importance scale: 0–3 trivia, 4–6 useful context, 7–8 decisions/preferences, 9–10 load-bearing facts you must not forget.

## Scientific papers / document corpus workflow

engram is purpose-built for ingesting documents and answering questions against them with real citations.

Create a KB/domain first:

```bash
engram kb create ageing-biology --description "Ageing biology and longevity science"
```

### Ingest once

```bash
# One PDF:
engram ingest ./paper.pdf --kb ageing-biology --mode papers --compile evidence

# Preview a directory before writing. Agents should do this first:
engram ingest ./my_papers/ --kb ageing-biology --mode papers --dry-run --include '*.pdf' --json

# Scoped directory ingest after the preview looks right:
engram ingest ./my_papers/ --kb ageing-biology --mode papers --include '*.pdf' --max-files 50 --compile evidence

# Curated takeaways / notes:
engram ingest ./takeaways/ --kb ageing-biology --mode takeaways --include '*.md' --max-files 25 --compile evidence
```

Behind the scenes: pdf-extract → section-aware chunking (preserves `Methods > Cell Culture` style breadcrumbs) → Gemini Embedding 2 at 1536 dimensions → BLOBs in SQLite → cited claims, source spans, entities, relations, takeaways, and wiki pages.

### Directory ingest safety

Single-file ingest is exact: pass a file path and engram ingests only that file.

Directory ingest is intentionally scoped for agent safety. With the default
`ingest.require_scope=true`, an unscoped directory write is refused unless the
command includes one of `--dry-run`, `--include`, `--exclude`, `--max-files`, or
`--all`.

Use this decision rule:

```bash
# 1. Preview the directory and inspect data.matched_files.
engram ingest ./docs --kb project-docs --mode general --dry-run --include '*.md' --json

# 2. Ingest the exact same scope, ideally with a cap.
engram ingest ./docs --kb project-docs --mode general --include '*.md' --exclude 'private/*' --max-files 40 --compile evidence --json

# 3. Use --all only when the user explicitly wants the whole recursive folder.
engram ingest ./docs --kb project-docs --mode general --all --compile evidence --json
```

Configurable defaults, all overridden by explicit CLI flags:

```bash
engram config set ingest.require_scope true
engram config set ingest.include '*.pdf,*.md'
engram config set ingest.exclude 'private/*,drafts/*'
engram config set ingest.max_files 50
```

Equivalent env overrides: `ENGRAM_INGEST_REQUIRE_SCOPE`,
`ENGRAM_INGEST_INCLUDE`, `ENGRAM_INGEST_EXCLUDE`,
`ENGRAM_INGEST_MAX_FILES`.

### Update safety

Agents may check freshness, but should not mutate installs during ordinary
memory work:

```bash
# Safe bootstrap/maintenance check:
engram update --check --json

# Mutating update only when the user asks for it:
engram update --json
```

The JSON output includes `install_source`, `update_mode`, `release_url`,
`upgrade_command`, `can_execute_update`, and `post_update_commands`. Configure
repeated workflows with `update.mode` (`auto`, `check_only`, or `disabled`),
`update.channel` (`github` or `crates`), and optional `update.install_source`
(`homebrew`, `cargo`, `standalone`, or `source_build`). Equivalent env
overrides are `ENGRAM_UPDATE_MODE`, `ENGRAM_UPDATE_CHANNEL`,
`ENGRAM_UPDATE_INSTALL_SOURCE`, `ENGRAM_UPDATE_GITHUB_REPO`,
`ENGRAM_UPDATE_CRATE`, and `ENGRAM_UPDATE_HOMEBREW_FORMULA`.

Optional cloud compiler pass:

```bash
# Extraction: google/gemini-3.1-flash-lite-preview
# Synthesis:  google/gemini-3.1-pro-preview
engram compile --kb ageing-biology --all --llm --max-llm-chunks 25
```

Use `--llm` when the user wants higher-recall scientific extraction or synthesized wiki overview pages. It requires `OPENROUTER_API_KEY`; deterministic compilation still works without it.

### Query any time

```bash
# Short, meaningful questions work best. Use natural language.
engram recall "what does HippoRAG do for multi-hop retrieval" --kb ageing-biology --mode evidence --profile cloud_quality --top-k 5 --json
engram research "what human evidence exists for rapamycin dosing?" --kb ageing-biology --json

# If the user asks a question about a paper, ALWAYS recall first, then
# answer using the returned chunk content as your evidence. Do NOT
# paraphrase from training data when verifiable content is in memory.
```

Each result has `id`, `kind`, `score`, `content`, citations, KB, and `sources` (`dense`, `lexical`, `entity`, `graph`, `reranker`). Cite the source/chunk_id when you quote the content, so the user can re-run `engram recall` to verify.

### Typical scientific workflow in one session

```bash
# User: "summarize the mTOR section of that rapamycin paper I gave you"
engram recall "mTOR rapamycin methods" --kb ageing-biology --top-k 3 --json
# → read the returned chunks, summarize using ONLY their content, cite chunk_ids.

# User: "how does that compare to metformin?"
engram recall "metformin mechanism longevity" --kb ageing-biology --top-k 3 --json
# → compare the two retrieved sets in your reply.

# User: "remember that we concluded rapamycin is more potent at 10 nM"
engram remember "Conclusion: rapamycin is more potent than metformin at 10 nM (user stated 2026-04-08)." --importance 8 --tag decision
```

## Specialist diaries (per-agent namespaces)

If multiple agents share the same engram store, give each its own diary so contexts don't bleed:

```bash
engram remember "..." --diary code-reviewer
engram recall "..." --diary code-reviewer        # only code-reviewer's memory
engram recall "..." --diary '*'                   # all diaries at once
```

Diary names are free-form (`default`, `research`, `cs-tutor`, etc.). When in doubt, use `default`.

## Knowledge bases / domains

Use KBs to separate expertise domains:

```bash
engram kb create swiftui-ios --description "SwiftUI and iOS engineering"
engram kb create bioinformatics --description "Bioinformatics methods and tools"
engram kb create acme-support --description "Acme product support policies and troubleshooting"
engram recall "latest SwiftUI navigation pattern" --kb swiftui-ios --mode evidence --json
engram entities list --kb bioinformatics
engram graph neighbors mTORC1 --kb ageing-biology --hops 1
engram wiki --kb ageing-biology index.md
engram documents list --kb ageing-biology --json
engram jobs list --kb ageing-biology --json
engram usage --kb ageing-biology --json
engram budget show --kb ageing-biology --json
```

Use `--all-kbs` only when you intentionally want cross-domain retrieval. Existing legacy embeddings are refused by default; run `engram reindex --kb <name>` after changing embedding model/dimensions/prompt format.

## Domain recipes agents should follow

### Medicine / clinical research

```bash
engram kb create medical-research --description "Guidelines, papers, protocols, safety notes"
engram ingest ./papers --kb medical-research --mode papers --include '*.pdf' --max-files 50 --compile evidence
engram ingest ./takeaways --kb medical-research --mode takeaways --include '*.md' --max-files 25 --compile evidence
engram recall "contraindications and evidence for intervention X" --kb medical-research --mode evidence --json
```

Use retrieved chunks as evidence. Distinguish papers, guidelines, preprints, reviews, and user notes. Do not turn retrieved evidence into diagnosis or treatment instructions without appropriate clinical review.

### Science / research papers

```bash
engram kb create ageing-biology --description "Ageing biology, longevity, mTOR, AMPK, senescence"
engram ingest ./ageing-papers --kb ageing-biology --mode papers --include '*.pdf' --max-files 50 --compile evidence
engram recall "human rapamycin dosing evidence" --kb ageing-biology --mode evidence --profile cloud_quality --json
```

Prefer `--mode evidence` for factual answers, `--mode explore` for hypothesis generation, and `engram wiki --kb K index.md` when the user asks for a domain overview.

### Coding/tool specialist

```bash
engram kb create swiftui-ios --description "SwiftUI, iOS, Apple docs, app architecture decisions"
engram ingest ./docs --kb swiftui-ios --mode general --include '*.md' --max-files 50 --compile evidence
engram ingest ./release-notes --kb swiftui-ios --mode takeaways --include '*.md' --max-files 25 --compile evidence
engram ingest ./repo-notes --kb swiftui-ios --mode takeaways --include '*.md' --max-files 25 --compile evidence
engram recall "NavigationStack migration gotchas" --kb swiftui-ios --mode agent --json
```

Use this when the user wants the agent to know a tool, framework, local repo, or product API. Recall before giving version-sensitive implementation advice.

### Customer support / product knowledge

```bash
engram kb create acme-support --description "Help center, policies, macros, resolved ticket lessons"
engram ingest ./help-center --kb acme-support --mode general --include '*.md' --max-files 100 --compile evidence
engram ingest ./support-takeaways --kb acme-support --mode takeaways --include '*.md' --max-files 25 --compile evidence
engram recall "refund policy for annual plans" --kb acme-support --mode evidence --json
```

Use KBs for product documentation and policies. Use diaries for support-agent preferences or team operating memory. Do not save secrets, payment details, or unnecessary customer PII.

### Personal/project memory

```bash
engram recall "project release preferences" --mode agent --json
engram remember "Decision: publish Homebrew only after the installed binary smoke test passes." --importance 9 --tag decision
```

Use `remember` for durable facts and explicit decisions. Use `ingest` for larger source corpora.

## Discovery

```bash
engram agent-info
```

Returns the full manifest: every command, every flag, exit codes, envelope shape, config path, supported providers. **Read it once per session** and never guess at command syntax.

## JSON envelope (how to parse output)

```jsonc
// success
{
  "version": "1",
  "status": "success",                            // or "no_results", "partial_success"
  "data": { /* command-specific */ },
  "metadata": { "elapsed_ms": 342, "retriever": "hybrid_gemini", "reranker": "cohere" }
}

// error — always on stderr, never on stdout
{
  "version": "1",
  "status": "error",
  "error": {
    "code": "bad_input",                          // machine-readable
    "message": "query cannot be empty",           // human
    "suggestion": "Run `engram --help` for argument syntax.",  // actionable
    "exit_code": 3
  }
}
```

**Exit codes tell you what to do next:**

| Code | Meaning | Your action |
|---|---|---|
| 0 | Success | Use the data |
| 1 | Transient (IO/net) | Retry with backoff |
| 2 | Config error | Tell user to run `engram config check` |
| 3 | Bad input | Fix your arguments, don't retry identically |
| 4 | Rate limited | Wait, then retry |

## Setup (first time only)

```bash
engram config check                                    # see what's configured
engram config set keys.gemini $YOUR_GEMINI_API_KEY       # Gemini Embedding 2
engram config set keys.cohere $YOUR_COHERE_API_KEY       # Cohere rerank-v3.5
engram config set keys.openrouter $YOUR_OPENROUTER_KEY   # optional synthesis/fact extraction
engram config set compiler.extraction_model google/gemini-3.1-flash-lite-preview
engram config set compiler.synthesis_model google/gemini-3.1-pro-preview
engram doctor --json
```

Keys are resolved in order: explicit env var → config TOML → none. If Gemini is not set, recall falls back to a deterministic offline stub (useful for tests, useless for real quality). Full secrets are never printed.

## Commands you'll actually use

| Command | Purpose |
|---|---|
| `engram recall "<q>"` | Hybrid search. The primary memory-read operation. |
| `engram kb create/list/show/delete` | Manage domain knowledge bases. |
| `engram remember "<text>" --importance N --tag T --diary D --kb K` | Store a fact. |
| `engram ingest <path> --kb K --mode papers\|takeaways\|conversations\|repos\|general\|auto --compile evidence` | Mine one file, or a scoped directory with `--dry-run`, `--include`, `--exclude`, `--max-files`, or explicit `--all`. |
| `engram documents list/show/delete` | Inspect or remove ingested source documents. |
| `engram compile --kb K --all` | Regenerate claims/entities/relations/takeaways/wiki. Add `--llm` for Gemini 3.1 Flash-Lite extraction + Gemini 3.1 Pro synthesis. |
| `engram research "<q>" --kb K --json` | Evidence-first research pass with query plan and citation readiness. |
| `engram jobs list/show` | Inspect compile job history. |
| `engram reindex --kb K` | Re-embed a KB with the active embedding model. |
| `engram doctor --json` | Check keys/schema/embedding consistency/daemon port. |
| `engram serve` | Start local HTTP API on 127.0.0.1:8768. |
| `engram usage --kb K --json` | Summarize recorded Gemini/Cohere usage. |
| `engram budget show/set/clear` | Inspect or set local API usage guardrails. |
| `engram edit <id> --content "..." --importance N` | Update a stored memory. |
| `engram forget <id> --confirm` | Destructive — requires `--confirm`. |
| `engram export > backup.json` / `engram import backup.json` | Backup / restore. |
| `engram entities list` / `engram entities show <name>` | Browse the entity graph. |
| `engram bench longmemeval --limit 100` | Verify retrieval quality against LongMemEval. |
| `engram config show\|set\|check` | Configuration. |
| `engram update --check` | Report current/latest version, install source, and runnable upgrade command. |
| `engram agent-info` | Self-describing manifest (read this first). |

## When NOT to use engram

- Don't dump every turn of conversation into it — store only what's worth keeping across sessions.
- Don't use it for task-local state that disappears when the task ends.
- Don't use it as a general key-value store; it's optimized for natural-language content with semantic retrieval.
- Don't save secrets, PII, or anything the user hasn't agreed to persist.

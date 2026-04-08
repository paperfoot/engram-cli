---
name: engram
description: Persistent memory for AI agents. Use BEFORE answering to recall prior facts/decisions, and AFTER learning something new to remember it. Also the knowledge engine for ingested documents (papers, repos, notes) — shell out to `engram` for hybrid semantic + keyword retrieval. Run `engram agent-info` once to discover every command.
---

# engram — how to actually use it

`engram` is a persistent, hybrid-retrieval memory CLI. It is not an MCP server and not a web service; it is a single binary named `engram` on your PATH. Shell out to it like you would to `gh` or `jq`.

## The memory loop (do this every session)

Whenever you start working on a task for a user:

```bash
# 1. LOAD — pull anything you already know that might be relevant.
engram recall "<user's task in 4-6 words>" --top-k 5 --json
```

Read the returned chunks. They contain prior facts, decisions, preferences, or document excerpts that should inform how you answer. If nothing comes back (`status: no_results`), proceed without memory but be extra diligent about saving what you learn.

```bash
# 2. WORK — do the task. Cite the recalled chunks by their chunk_id when relevant.

# 3. SAVE — anything the user told you that will matter later:
engram remember "Elias prefers Rust over Go for CLI tools because of single-binary deployment." --importance 7 --tag preference
engram remember "Decision 2026-04-08: use LanceDB-style BLOB embeddings in SQLite instead of Qdrant." --importance 9 --tag decision
```

**Rule of thumb for what to save:**
- **Always save:** user preferences, explicit decisions with rationale, stable facts about people/projects, corrections the user made to you.
- **Sometimes save:** interesting findings from research that will be reused, links worth remembering, error patterns that solved a bug.
- **Never save:** ephemeral task state, things the user can re-tell you, conversation filler.

Importance scale: 0–3 trivia, 4–6 useful context, 7–8 decisions/preferences, 9–10 load-bearing facts you must not forget.

## Scientific papers / document corpus workflow

engram is purpose-built for ingesting documents and answering questions against them with real citations.

### Ingest once

```bash
# One PDF:
engram ingest ./paper.pdf --mode papers

# A whole directory (recursive, handles PDF + txt + md):
engram ingest ./my_papers/ --mode papers
```

Behind the scenes: pdf-extract → section-aware chunking (preserves `Methods > Cell Culture` style breadcrumbs) → Gemini Embedding 2 (batched, token-budgeted) → BLOBs in SQLite. Chunks are embedded once and persist forever.

### Query any time

```bash
# Short, meaningful questions work best. Use natural language.
engram recall "what does HippoRAG do for multi-hop retrieval" --top-k 5 --json

# If the user asks a question about a paper, ALWAYS recall first, then
# answer using the returned chunk content as your evidence. Do NOT
# paraphrase from training data when verifiable content is in memory.
```

Each result has a `chunk_id`, `score`, `content`, and `sources` array (`["dense","lexical","reranker"]`). Cite the chunk_id when you quote the content, so the user can re-run `engram recall` to verify.

### Typical scientific workflow in one session

```bash
# User: "summarize the mTOR section of that rapamycin paper I gave you"
engram recall "mTOR rapamycin methods" --top-k 3 --json
# → read the returned chunks, summarize using ONLY their content, cite chunk_ids.

# User: "how does that compare to metformin?"
engram recall "metformin mechanism longevity" --top-k 3 --json
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
engram config set keys.gemini $YOUR_GEMINI_API_KEY     # required for hybrid retrieval
engram config set keys.cohere $YOUR_COHERE_API_KEY     # optional, ~4pt R@1 lift
```

Keys are resolved in order: explicit env var → `~/.config/engram/config.toml` → none. If Gemini is not set, recall falls back to a deterministic offline stub (useful for tests, useless for real quality).

## Commands you'll actually use

| Command | Purpose |
|---|---|
| `engram recall "<q>"` | Hybrid search. The primary memory-read operation. |
| `engram remember "<text>" --importance N --tag T --diary D` | Store a fact. |
| `engram ingest <path> --mode papers\|conversations\|repos\|general\|auto` | Mine a file or directory. |
| `engram edit <id> --content "..." --importance N` | Update a stored memory. |
| `engram forget <id> --confirm` | Destructive — requires `--confirm`. |
| `engram export > backup.json` / `engram import backup.json` | Backup / restore. |
| `engram entities list` / `engram entities show <name>` | Browse the entity graph. |
| `engram bench longmemeval --limit 100` | Verify retrieval quality against LongMemEval. |
| `engram config show\|set\|check` | Configuration. |
| `engram agent-info` | Self-describing manifest (read this first). |

## When NOT to use engram

- Don't dump every turn of conversation into it — store only what's worth keeping across sessions.
- Don't use it for task-local state that disappears when the task ends.
- Don't use it as a general key-value store; it's optimized for natural-language content with semantic retrieval.
- Don't save secrets, PII, or anything the user hasn't agreed to persist.

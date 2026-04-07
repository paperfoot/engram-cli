---
name: engram
description: Persistent agent memory + scientific knowledge engine. Use to store, retrieve, and ingest content. Run `engram agent-info` for the full capability manifest.
---

# engram

`engram` is a local-first, agent-native memory CLI. The binary IS the interface — there is no MCP server, no web UI, no separate service. Everything you need is exposed through subcommands that emit JSON when piped or with `--json`.

## How to discover capabilities

```bash
engram agent-info
```

This returns a JSON manifest describing every command, flag, exit code, and the JSON envelope shape. Use it as your single source of truth.

## Most-used commands

```bash
engram remember "Rapamycin extends mouse lifespan."   # store a fact
engram recall "rapamycin lifespan" --top-k 5 --json   # retrieve relevant
engram ingest ./papers/ --mode papers                 # mine a directory
engram bench longmemeval                              # measure quality
```

## Conventions

- Output is JSON when stdout is piped or `--json` is set; human tables otherwise.
- Errors go to stderr with `code`, `message`, `suggestion`, and a semantic `exit_code`.
- Exit codes: 0 success, 1 transient (retry), 2 config (fix setup), 3 bad input (fix args), 4 rate limited (back off).
- Configuration via env vars: `GEMINI_API_KEY`, `COHERE_API_KEY`. Run `engram config check`.
- Diary namespaces: pass `--diary <name>` to keep separate memory contexts per specialist agent.

## When to use it

- You need to remember anything across conversations.
- You're processing scientific papers and want hybrid retrieval over them.
- You want a local-first knowledge store you can grep, ingest, and query without running services.

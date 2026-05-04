# Engram Domain Expertise Engine Verification - 2026-04-27

## Build

- Branch: `feat/mlx-reranker`
- Date: 2026-04-27
- Goal: implement KB/domain expertise engine with cloud-quality retrieval defaults.

## Configuration Checks

Command:

```bash
target/debug/engram doctor --json
```

Result:

- `ok: true`
- Gemini key: present
- Cohere key: present
- OpenRouter key: present
- Schema version: 2
- Embedding consistency: true
- Embedding profile: `gemini-embedding-2`, `1536`, `gemini-embedding-2-v1`
- Daemon port `127.0.0.1:8768`: available

## Reindex

Command:

```bash
target/debug/engram reindex --all --json
```

Result:

- Chunks reindexed: 304
- Model: `gemini-embedding-2`
- Dimensions: 1536
- Prompt format: `gemini-embedding-2-v1`

The migrated `default` KB was also compiled after reindex:

```bash
target/debug/engram compile --kb default --all --json
```

Result:

- Chunks scanned: 304
- Source spans: 304
- Claims: 371
- Entities: 967
- Relations: 1790
- Wiki pages: 51

## Smoke KB

Commands:

```bash
target/debug/engram kb create ageing-biology-smoke \
  --description "Smoke-test KB for ageing biology evidence" --json

target/debug/engram ingest benchmarks/fixtures/ageing-smoke \
  --kb ageing-biology-smoke \
  --mode takeaways \
  --compile evidence \
  --json
```

Result:

- Memories created: 1
- Chunks created: 7
- Embedder: `gemini-embedding-2`
- Dimensions: 1536
- Prompt format: `gemini-embedding-2-v1`
- Claims: 7
- Entities: 8
- Relations: 3
- Source spans: 7
- Takeaways: 1
- Wiki pages: 9

## Cloud-Quality Recall

Command:

```bash
target/debug/engram recall "rapamycin human vaccine response evidence" \
  --kb ageing-biology-smoke \
  --mode evidence \
  --profile cloud_quality \
  --top-k 5 \
  --json
```

Result:

- Metadata profile: `cloud_quality`
- Embed model: `gemini-embedding-2`
- Embed dims: 1536
- Prompt format: `gemini-embedding-2-v1`
- Reranker: `cohere/rerank-v3.5`
- Candidates considered: 50
- Returned cited chunks and claims from `benchmarks/fixtures/ageing-smoke/rapamycin-human-takeaways.md`

## Entity, Graph, Wiki

Commands:

```bash
target/debug/engram entities list --kb ageing-biology-smoke --json
target/debug/engram entities show Rapamycin --kb ageing-biology-smoke --json
target/debug/engram graph neighbors Rapamycin --kb ageing-biology-smoke --hops 1 --json
target/debug/engram wiki --kb ageing-biology-smoke index.md --json
```

Result:

- Entity list returned `Rapamycin`, `mTORC1`, `Human`, and related smoke fixture entities.
- `Rapamycin` graph neighbor returned `Rapamycin --co_occurs_with--> mTORC1`.
- Wiki `index.md` returned generated Markdown with entity links and chunk-derived content.

## Daemon/API

Commands:

```bash
target/debug/engram serve --host 127.0.0.1 --port 8768 --json
curl -s http://127.0.0.1:8768/health
curl -s -X POST http://127.0.0.1:8768/v1/recall \
  -H 'content-type: application/json' \
  --data '{"query":"rapamycin human vaccine response","kb":"ageing-biology-smoke","mode":"evidence","profile":"cloud_quality","top_k":3}'
curl -s -X POST http://127.0.0.1:8768/v1/reindex \
  -H 'content-type: application/json' \
  --data '{"kb":"ageing-biology-smoke"}'
curl -s -X POST http://127.0.0.1:8768/v1/ingest \
  -H 'content-type: application/json' \
  --data '{"path":"benchmarks/fixtures/ageing-smoke","kb":"api-ingest-smoke","mode":"takeaways","compile":"evidence"}'
```

Result:

- `/health` returned `ok: true`, schema version 2.
- `/v1/recall` returned cited chunk/claim results with Cohere rerank metadata.
- `/v1/reindex` reindexed 7 smoke KB chunks with Gemini Embedding 2.
- `/v1/ingest` ingested and compiled a smoke KB through the daemon path.
- `target/debug/engramd` alias started successfully and returned `/health`.
- Daemon process was stopped after verification.

## Benchmarks

Commands:

```bash
target/debug/engram bench mini --json
target/debug/engram bench mini-fts --json
```

Result:

- `mini`: R@1 0.90, R@5 1.00, R@10 1.00, MRR 0.95
- `mini-fts`: R@1 0.90, R@5 1.00, R@10 1.00, MRR 0.95

## Tests

Command:

```bash
cargo test --workspace
```

Result:

- 69 tests passed.
- Includes new integration coverage for KB ingest/compile/entity flow and mixed-embedding refusal.

## Notes

- The compiler implemented here is deterministic evidence compiler v1. It creates cited claims, entities, co-occurrence relations, takeaways, and wiki pages without requiring an LLM call. OpenRouter is detected and remains available for future synthesis/extraction upgrades.
- SQLite remains the only graph/knowledge store. No Neo4j, Kuzu, or external graph service is required.

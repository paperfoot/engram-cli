---
name: engram
description: Personal, business, and project memory for AI agents — durable facts, decisions, relationships, competitor/market intel, and project context. Use BEFORE answering to recall prior facts/decisions, and AFTER learning something durable to remember it. This is for PERSONAL/BUSINESS knowledge, NOT scientific-paper corpora — for research literature (papers, neuromodulation/medical corpora) use the superlibrary skill instead. Shell out to `engram` for hybrid semantic + keyword retrieval. Run `engram agent-info` once to discover every command.
compatibility: Requires the `engram` CLI on PATH. Works with Claude Code, Codex, Gemini CLI, and Agent Skills compatible clients.
metadata:
  homepage: "https://github.com/paperfoot/engram-cli"
  package: "engram-skill.zip"
---

# engram

A binary on PATH (not an MCP server). `engram agent-info` is the full manifest — read it once per session instead of guessing syntax.

## The memory loop

```bash
# BEFORE answering anything that might depend on what you already know:
engram recall "<the user's question in 4-6 words>" --json

# AFTER learning something durable:
engram remember "Sarah prefers async standups; sync only for incidents." --kb personal --importance 8
engram remember "Chose Postgres over DynamoDB: relational reporting beat write throughput." --kb business --importance 9 --tag decision
```

That is the whole loop. Everything below is detail.

## Recall searches everything — don't narrow it

`engram recall "<query>"` spans **every KB**. No `--kb` needed, and adding one to "find" something is a mistake: you cannot know which namespace a fact was filed under, because the write chose it, not you.

- `--kb <name>` — narrow deliberately, to *exclude* the rest (e.g. business-only competitor intel).
- `--all-kbs` or `--kb '*'` — force everything, even when `retrieval.default_kb` is pinned.

`metadata.kb_scope` says how the scope was chosen (`flag` | `config` | `all_kbs`) and `metadata.kb` gives the effective KB (`*` = everything). **If a recall comes back empty, read those two before telling the user nothing is stored.**

## KBs vs diaries — don't mix them

- **KBs** are domains: `personal` (people, family, health, preferences), `business` (company, competitors, deals), one per project, plus source corpora like `ageing-biology`. Writing with `--kb <name>` registers it — no `kb create` first.
- **Diaries** are agent/user namespaces (`default`, `code-reviewer`). When several agents share a store, give each its own `--diary`; `--diary '*'` searches all. Diary-scoped recall reports whatever the isolation guard withheld in `metadata.diary_isolation`.

## Dates: use `--event-time`

Anything with a real date — birthdays, anniversaries, when a decision was made — needs `--event-time` (RFC3339 or `YYYY-MM-DD`), or it is only ever findable by wording:

```bash
engram remember "Mum's birthday is 14 March." --kb personal --event-time 1958-03-14 --importance 9
```

Then time works: `engram recall "what happened last March?" --since 2026-03-01 --until 2026-03-31`. Both filters read `event_time` and fall back to write time — so a dated memory saved without it looks like it happened the day you typed it.

## What to save (and not)

- **Always:** user preferences, explicit decisions with rationale, stable facts about people/projects, corrections the user made to you.
- **Sometimes:** reusable findings, links worth remembering, error patterns that solved a bug.
- **Never:** ephemeral task state, conversation filler, things the user can re-tell you, secrets, payment details, PII, anything the user hasn't agreed to persist. Not a key-value store.

Importance: 0–3 trivia, 4–6 useful context, 7–8 decisions/preferences, 9–10 load-bearing facts.

## Facts and contradictions

`remember` extracts `(subject, predicate, object)` facts automatically when an OpenRouter key resolves. **Check `fact_extraction` in the response** — `skipped_no_openrouter_key` means nothing was captured. Opt out with `--no-extract-facts`; skip the LLM call by supplying the triple yourself via `--facts-json '[{"subject":"...","predicate":"...","object":"..."}]'` (same contradiction detection, zero cost). For memories stored before extraction was on: `engram facts backfill --kb personal [--dry-run] [--limit 50]` — idempotent, resumes where it stopped.

When `remember` returns a non-empty `adjudication_needed`, **you** are the adjudicator — you have the conversation context engram lacks. Run each entry's `resolve_command` with `--action supersede-old` | `keep-both` | `reject-new`, plus `--note "why"`. If you genuinely can't tell, leave it queued and say so. Safety slots (`allergic_to`, `takes_medication`, `diagnosed_with`) are never auto-superseded.

`recall` output carries a `conflicts` array: **superseded** (a fact was updated — cite the current value and note the change) and **contested** (unresolved; both values listed — never pick one silently).

## The networking view

People are first-class entities with typed relationships and provenance:

```bash
engram people list --kb '*' --min-mentions 2        # who you know: aliases, mention counts, top relations
engram people show "Sarah Chen" --kb '*'            # relations WITH sources + the memories mentioning her
engram graph neighbors "Sarah Chen" --kb personal --typed-only --limit 20
```

`--typed-only` drops weak `co_occurs_with` edges, leaving the real relationship graph rather than co-mention noise. `people show` resolves any known alias, case-insensitively.

## Quality gate

`engram bench personal --json` is offline and deterministic — hash embedder, no network, no keys, and it never touches your store. Run it before and after any retrieval change. Headline metrics are production behaviour; `legacy_default_comparison` is what the old single-KB default would have scored, kept as a regression tripwire.

## Document corpora (secondary)

For ingested source corpora — PDFs, product docs, support archives — the rules tighten. **For research literature (papers, neuromodulation/medical corpora) use the superlibrary skill instead.**

Recall first, answer only from returned chunks, and cite each claim's `source`/`chunk_id` so the user can re-run the recall to verify. On `status: no_results` or only low-scoring chunks, say "insufficient evidence in the knowledge base" — never fill from training data or invent a citation. If sources conflict, cite both. For "how many / list all" questions pass `--top-k 30–50`, synthesize over the whole returned set rather than the first few, and union the hits from synonym and acronym variants.

With the default `ingest.require_scope=true` an unscoped directory ingest is refused: run `--dry-run --include '...' --json` first, inspect `data.matched_files`, then re-run the same scope with `--max-files` (`--all` only when the user explicitly wants the whole folder). Single-file ingest is exact and unaffected. `engram compile --kb K --all` derives claims, entities and wiki pages; `engram research "<q>" --kb K` is the evidence-first pass.

## Gotchas

- **Errors are JSON on stderr**, never stdout. Exit codes: 1 transient (retry with backoff), 2 config (`engram config check`), 3 bad input (fix args, don't retry identically), 4 rate limited (wait).
- Every command returns `{version, status, data, metadata}`; `--json` is automatic when stdout is piped.
- **Memories missing from `engram kb list`?** Older builds wrote to KBs without registering them. Run `engram kb repair --dry-run`, then without the flag. Additive, idempotent, and it won't resurrect a KB you deleted.
- **After changing embedding model/dimensions/prompt format, run `engram reindex --kb <name>`** — legacy embeddings are refused by default.
- `engram forget <id>` requires `--confirm`. `engram update` (without `--check`) mutates the install — only on user request.
- `engram skill install` refuses to overwrite a SKILL.md you edited by hand; pass `--force` if you mean it.
- No Gemini key configured → recall silently falls back to a deterministic offline stub (fine for tests, useless for quality). `engram doctor --json` to verify; `--integrity` also flags unregistered KBs.

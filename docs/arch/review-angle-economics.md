The single most important missing thing is a real wire-level attention budget. `token_budget` currently measures retrieved text, not what the agent actually receives. A routine recall can report 760 of 800 tokens used while emitting 5,396 tokens of JSON.

Measurements below are exact bytes from `engram 0.4.0`; token counts use a local Qwen 3.5 tokenizer as a reproducible reference. Other frontier tokenizers will vary, but the scale will not.

| Agent-visible surface | Scenario | Bytes | Reference tokens |
|---|---:|---:|---:|
| `agent-info` | Pretty-printed default | 143,723 | 37,420 |
| Installed `SKILL.md` | Always-loaded skill | 22,880 | 5,738 |
| `recall` | 10 hits, ~300 chars each | 15,797 | 5,531 |
| `state` | 25 current facts | 29,002 | 12,496 |
| `state` | 50 current facts | 57,235 | 24,748 |
| `facts list` | Default 50 rows | 48,680 | 20,730 |
| `state` stress case | Allowed default ceiling of 200 | 222,778 | 96,046 |

The response fixture used 60 ordinary memories, 30 matching chunks, and a 50-fact personal profile. The 200-fact row is a separate ceiling test, not claimed as everyday use.

## 1. The advertised token budget excludes most of the bill

Recall budgets only `approx_tokens(&r.content)`, using chars÷4, then deliberately returns at least `top_k` even when that exceeds the budget. It subsequently adds the second copy of every result, identifiers, provenance, conflicts and two metadata blocks. See [recall.rs](/Users/dev/Projects/engram-cli/crates/engram-cli/src/commands/recall.rs:130), [layers.rs](/Users/dev/Projects/engram-cli/crates/engram-core/src/layers.rs:25), and [recall.rs](/Users/dev/Projects/engram-cli/crates/engram-cli/src/commands/recall.rs:344).

For the measured 10-hit recall:

- `answer_context`: 5,312 bytes / 1,565 tokens.
- Structured `results`: 9,452 bytes / 3,647 tokens.
- The actual retrieved content, once: 3,244 bytes / 542 tokens.
- Seven metadata fields appear both inside `data.metadata` and the outer envelope.

Thus most of the agent’s payment is framing, UUIDs, provenance and a second representation of identical content. The quarantine notice is valuable, but it does not require returning both XML-like context and full structured rows. [retrieval.rs](/Users/dev/Projects/engram-cli/crates/engram-cli/src/retrieval.rs:311)

`state` and `facts list` have no output-token budget at all. `state` defaults to 200 values, repeats up to 240 characters of provenance snippet per fact, and emits confidence decomposition plus multiple identifiers for every value. `facts list` averages roughly 967 bytes / 412 reference tokens per fact. See [cli.rs](/Users/dev/Projects/engram-cli/crates/engram-cli/src/cli.rs:325), [state.rs](/Users/dev/Projects/engram-cli/crates/engram-cli/src/commands/state.rs:430), and [facts.rs](/Users/dev/Projects/engram-cli/crates/engram-cli/src/commands/facts.rs:1331).

What I would do: budget the final serialized response, after all framing. Separate `content_tokens` from `output_tokens`; make the latter the binding ceiling. A response that cannot fit must paginate or return `truncated:true` with a continuation—not silently overrun context.

## 2. Diaries are namespaces, not a collaboration or sync model

For two agents on one store, diaries provide isolation, but there is no agent-facing changefeed, acknowledgment cursor, shared/private publication step or “what changed since my last turn?” surface. The journal exists, yet an agent can only rediscover another agent’s changes by guessing a suitable recall query.

For two machines there is no convergence protocol:

- Events have local `seq`, UUID, timestamp and actor, but no stable store/replica identity, causal parent or revision number. [journal.rs](/Users/dev/Projects/engram-cli/crates/engram-storage/src/journal.rs:43)
- The export format explicitly says sequence is local and “two merged slices cannot share one.” [export_v3.rs](/Users/dev/Projects/engram-cli/crates/engram-storage/src/export_v3.rs:51)
- Export is a full snapshot; the CLI has no `since_seq`, replica cursor or push/pull protocol. [cli.rs](/Users/dev/Projects/engram-cli/crates/engram-cli/src/cli.rs:471)

I tested the only plausible user workaround. Two temporary stores started from the same memory, independently edited it to “Zed” and “VS Code,” then cross-imported their v3 exports. Both stores ended with both revision event IDs, in opposite local order, while retaining different materialized contents. Since subsequent imports skip already-known event IDs, there is no remaining convergence step. This is not an import-trust finding; it demonstrates that export/import is backup/restore, not sync.

Typed handles also lack store and revision identity. The same `mem:<uuid>` can therefore name divergent content on two replicas with no way for an agent to detect that its handle is stale.

Near term, I would formally support one canonical daemon per user and make other machines clients of it. If offline multi-master use is required, engram needs `store_id`, `replica_id`, causal/HLC ordering, deterministic merge rules, revision-qualified handles, incremental journal exchange and a compact `changes --after <cursor>` surface for agents.

## 3. “Append-only forever” has no operational lifecycle

The journal, current projections, chunks, embeddings, usage rows and job history all grow; forget/retract hides assertions but does not reclaim the underlying history. Even deleting a KB deliberately leaves its events behind. [rebuild.rs](/Users/dev/Projects/engram-cli/crates/engram-storage/src/rebuild.rs:617)

Every v3 export includes the complete event history plus KBs, documents, memories, chunks and facts. Therefore backup size and transfer cost grow monotonically, with stored text represented in multiple sections. [export_v3.rs](/Users/dev/Projects/engram-cli/crates/engram-storage/src/export_v3.rs:24)

There is nominal forgetting machinery—`access_count`, `last_accessed`, and an “Ebbinghaus stability parameter”—but I found no production path that updates or operationally consumes it. [types.rs](/Users/dev/Projects/engram-cli/crates/engram-core/src/types.rs:15) The paid-provider budget is useful, but it gates estimated API spend already incurred; it does not cap store bytes, derived-cache bytes, response tokens or history growth. [budget.rs](/Users/dev/Projects/engram-cli/crates/engram-cli/src/commands/budget.rs:126)

Over years, engram needs:

- Explicit retention classes: pinned, durable, decaying and ephemeral/TTL.
- `lifecycle status` and `lifecycle plan --dry-run`, showing bytes, event growth, cold candidates and projected backup size.
- Immutable archived journal segments plus verified checkpoints, allowing the hot store to compact without discarding auditability.
- Separate ceilings for hot-store size, derived caches, job history and agent-output tokens.
- A genuinely irreversible purge story when the user means erase, distinct from historical retraction.

Safety facts, adjudications and explicit decisions should be pinned by policy; routine agent observations should not default to permanent hot storage.

## 4. The manifest is enormous, yet the generated contract is shallow

The skill explicitly tells every agent to read the full manifest once per session. [skill.md](/Users/dev/Projects/engram-cli/crates/engram-cli/assets/skill.md:14) Skill plus pretty manifest costs approximately 43,158 reference tokens before useful work begins.

Within the minified 126 KB manifest:

- `command_schemas`: 71,858 bytes / 16,590 tokens.
- Hand-written command prose: 17,487 bytes / 4,238 tokens.
- Agent decision rules: 7,663 bytes / 1,716 tokens.

This is especially wasteful because `agent-info --schema <command>` already supports lazy discovery. The default manifest nevertheless injects fragments for all 36 generated command schemas. [agent_info.rs](/Users/dev/Projects/engram-cli/crates/engram-cli/src/agent_info.rs:493)

After paying that cost, the JSON Schema still treats the structures an agent actually consumes as opaque. For example, `recall.results.items`, `state.state.items`, `facts`, `conflicts`, provenance and job objects are merely `{"type":"object"}`. `Ty` has no named struct/reference type, so the generator cannot describe nested fields. [model.rs](/Users/dev/Projects/engram-cli/crates/engram-contract/src/model.rs:10), [jsonschema.rs](/Users/dev/Projects/engram-cli/crates/engram-contract/src/jsonschema.rs:91)

A manifest should be a compact capability index, not an embedded manual:

- Default: version, schema hash, core intents, command names, risk level and per-command schema references.
- `--full`: retain today’s exhaustive document.
- Cache contracts across sessions by `schema_hash`.
- Add named `$defs` for result, provenance, conflict, fact, state and job types so schemas can actually drive an agent or SDK.

## 5. A job’s execution semantics depend implicitly on daemon presence

The same `jobs submit` invocation has three materially different meanings:

- Daemon detected: return a queued handle.
- No daemon: run synchronously in the caller, potentially for minutes.
- No daemon plus `--detach`: create a queued job that nothing will run.

That behavior is explicit in [jobs.rs](/Users/dev/Projects/engram-cli/crates/engram-cli/src/commands/jobs.rs:1) and its dispatch path at [jobs.rs](/Users/dev/Projects/engram-cli/crates/engram-cli/src/commands/jobs.rs:76).

For an agent orchestration layer, this makes latency and completion semantics machine-dependent. A tool call expected to return a handle may instead occupy the process until a timeout; an apparently detached job may remain inert after the session ends. The returned job object has useful progress and resumability fields, but no explicit execution target, active worker/lease, spend-so-far or ETA. [jobs.rs](/Users/dev/Projects/engram-cli/crates/engram-storage/src/jobs.rs:99)

I would separate the contracts:

- `--execution inline`: definitely block.
- `--execution daemon`: definitely return a handle or fail `daemon_unavailable`.
- `--execution queued`: persist without claiming.
- Add `engram daemon ensure|status|stop`, with a store fingerprint and daemon instance ID.
- Return `execution`, `worker`, `lease_until`, cost budget/spend and the next runnable action in every job object.

The typed handles, structured conflict-resolution objects, `no_match` verdicts, totals/cursors, and store-free discovery are genuinely good primitives. They should survive unchanged in a smaller surface.

## The one change I would make tomorrow

Introduce one generated, global response profile: `--view agent --max-output-tokens N`, and make the installed skill use it. Keep today’s response as `--view audit`.

The agent view should emit each piece of content once, retain the trust warning once, use compact provenance references, omit audit-only/null fields, and enforce the ceiling against the final serialized response. `state` and listings should paginate under the same ceiling. Apply the same profile to `agent-info`, making it a compact cached capability index.

That single abstraction would immediately improve recall, state, facts, discovery and jobs without weakening the richer audit surface.

Review was read-only against the source unchanged from `4f1ad71`; the shared checkout subsequently advanced to docs-only HEAD `92c2437`. No workspace files were modified.


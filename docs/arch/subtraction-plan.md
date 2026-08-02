# Verdict

The highest-return subtraction is to stop sending the same knowledge twice. Make the agent wire compact by default, then delete the compiler-derived “second memory” rather than repairing its pseudo-epistemology.

I audited the clean `arch/v0.4` checkout at `6da723e`: it is now 93 commits over `main`, with 1,183 test attributes—not the brief’s earlier 88/1,181 snapshot. The workspace contains 72,409 Rust lines under production `src/`; a fresh v13 store has 28 logical tables, 44 SQLite table entries, and 294 columns.

The ordering below weighs measured impact, confidence, and implementation effort. Counts overlap where noted and should not be added blindly.

## 1. Make the wire single-copy and lazy

What dies:

- Default inclusion of all 36 command schemas in `agent-info`.
- `recall.data.answer_context`, because `results[].content` already contains the same text.
- Duplicate `id`/`handle`/`memory_id`, duplicate inner/outer metadata, null fields, provider/debug statistics, and ranking scores whose meaning varies by profile.
- Full diagnostic provenance in ordinary agent responses.

What replaces it:

- `agent-info` becomes a compact capability index: version, schema hash, command names, risk class, and schema references. The existing `agent-info --schema <command>` remains the source for exact arguments.
- `recall` sends each result once: `handle`, `kind`, `content`, `kb`, relevant time, and a compact citation. Keep one response-level trust warning, `verdict`, conflicts, absence information, and scope.
- Rich diagnostics move to an explicit audit/debug invocation, never dual-sent.

Measured wins:

- Current `agent-info`: **143,723 bytes / 37,422 Qwen tokens**.
- Existing top-level help: 2,646 bytes / 573 tokens. One `recall` schema: 7,000 bytes / 1,742 tokens. That conservative lazy path costs 2,315 tokens, **93.8% less** than loading everything.
- A compact capability-index prototype was 1,390 bytes / 412 tokens.
- Fresh 10-hit recall fixture: **16,704 bytes / 5,774 tokens** minified, delivering 679 content tokens. A compact single-copy response retaining handles, scope, citations, conflicts, and trust status was **4,922 bytes / 1,192 tokens**: **70.5% fewer bytes and 79.4% fewer tokens**.
- The duplication is explicit where `answer_context` and structured results are assembled together in [recall.rs](/Users/dev/Projects/engram-cli/crates/engram-cli/src/commands/recall.rs:344), while every schema is embedded in [agent_info.rs](/Users/dev/Projects/engram-cli/crates/engram-cli/src/agent_info.rs:95).

Axes: **lighter, faster, leaner, simpler, safer**. Safety improves because retrieved text has one canonical, JSON-delimited representation rather than two representations with different escaping rules. The trust warning itself remains load-bearing; its rationale is sound in [retrieval.rs](/Users/dev/Projects/engram-cli/crates/engram-cli/src/retrieval.rs:311).

Cost and compatibility: consumers of `answer_context` or verbose metadata break. Bump the response contract version. Offer the old shape for one release only, then delete it; permanent dual-format support would recreate the weight.

## 2. Delete the compile-derived knowledge subsystem

What dies:

- Commands: `compile`, `research`, `wiki`, `graph`, `entities`, and `people`.
- Retrieval candidates from claims, wiki pages, entities, and relations.
- Tables: `source_spans`, `claims`, `claim_evidence`, `entities`, `entity_aliases`, `relations`, `takeaways`, and `wiki_pages`.
- Both claims/wiki FTS structures: 10 additional virtual/shadow table entries.
- `wiki`, `explore`, and graph-oriented recall modes.
- The deterministic entity/person extraction machinery and LLM compiler.

What replaces it:

- `recall` returns source passages.
- `state` returns current/as-of structured facts.
- Agents synthesize transiently from those two surfaces; engram stops persisting a second, lossy interpretation of its own evidence.
- Keep fact extraction through `remember`; that path participates in interval and conflict machinery.

Measured wins:

- Whole-file lower bound: **8,935 production lines removed**:
  - 2,721 lines across the six CLI command files.
  - 6,214 lines across graph expansion, entity extraction, people analysis, hygiene, and gazetteer files.
  - Shared storage, retrieval, contract, migration, and test deletions are additional and not counted.
- Public agent read verbs collapse from six to two: `recall` and `state`. `facts show` becomes an operator/audit view, not another recommended read contract.
- Every LLM compilation currently performs one extraction call per selected chunk plus one synthesis call—**N+1 provider round trips**—inside a transaction begun before compilation. See [compile.rs](/Users/dev/Projects/engram-cli/crates/engram-cli/src/commands/compile.rs:289) and the provider loop at [compile.rs](/Users/dev/Projects/engram-cli/crates/engram-cli/src/commands/compile.rs:664).
- Claim “grounding” is only numeric containment plus a 50% content-word overlap threshold, which cannot detect polarity reversal ([compile.rs](/Users/dev/Projects/engram-cli/crates/engram-cli/src/commands/compile.rs:1193)).
- Wiki citation validation proves only that a referenced chunk ID appeared in the supplied set ([compile.rs](/Users/dev/Projects/engram-cli/crates/engram-cli/src/commands/compile.rs:743)).
- `research.ready_for_synthesis` is merely `cited > 0`, even for irrelevant evidence ([research.rs](/Users/dev/Projects/engram-cli/crates/engram-cli/src/commands/research.rs:194)).

Axes: **safer, leaner, faster, lighter, simpler**.

Cost: this deliberately removes generated wikis, relationship browsing, entity aggregation, and the research façade. Those are real features. The argument for deletion is that they currently look more trustworthy than they are, compete directly with source passages, and would require most of the proposed v0.5 epistemic architecture to become honest.

Migration: these tables are already excluded from v3 export as regenerable caches. Offer a one-time Markdown/JSON dump for users who value generated pages, then drop the tables. Removed commands should fail with a concise “use `recall` or `state`” error for one release—not remain as aliases.

## 3. Remove dead schema and six speculative memory fields

What dies immediately:

- `edges`: DDL exists, but no SQL reader or writer.
- `document_sources`: only touched by an old dedup migration; never inserted or read.
- `embedding_runs`: no reader or writer.
- `entities_v1_global`: migration staging data copied but never dropped.
- Memory columns `emotional_weight`, `access_count`, `last_accessed`, `stability`, `valid_from`, and `valid_until`.

The first three tables are visible together in [sqlite.rs](/Users/dev/Projects/engram-cli/crates/engram-storage/src/sqlite.rs:531). The legacy entity table remains after copying at [sqlite.rs](/Users/dev/Projects/engram-cli/crates/engram-storage/src/sqlite.rs:1323).

Measured wins:

- Four logical tables and **25 columns** disappear: logical table count 28→24.
- Removing the six memory fields brings the total to **31 columns removed**, 294→263.
- Seven schema objects representing the four dead tables and their indexes occupy 1,578 bytes of fresh `sqlite_master` SQL.
- A default exported memory row fell from 427 to 285 bytes when those six fields were removed: **142 bytes saved per memory**.
- `admin rebuild` currently excludes memories specifically because three of these unused values are not journalled ([rebuild.rs](/Users/dev/Projects/engram-cli/crates/engram-storage/src/rebuild.rs:74)). Removing them strengthens the “journal can rebuild projections” claim.
- There is also a concrete performance bug: renaming `entities` to `entities_v1_global` carries `idx_entities_kind` with it. The attempted `CREATE INDEX IF NOT EXISTS` on the new table is then skipped, leaving the live `entities` table without that index.

Axes: **leaner, lighter, faster, simpler, safer**.

Cost: dormant legacy edge/source/run data and an unimplemented Ebbinghaus/decay design are discarded. `memory.valid_from` currently duplicates `event_time` or `created_at`; `memory.valid_until` has no production close path. Fact intervals remain untouched.

Migration: take the existing safe SQLite backup first, drop the staging tables, and recreate `idx_entities_kind` on the live table if proposal 2 has not already removed entities. Older export readers should accept and ignore the six fields; new exports stop writing them.

## 4. Delete `lessons` and `reflect`

What dies:

- `lessons add/list/confirm/retract`.
- `reflect`.
- The 15-column `lessons` table.
- **1,877 production lines** across the two CLI modules and storage implementation.
- Five embedded command schemas totalling **9,416 minified bytes**.

Why deletion is safer:

- `lessons add` assumes whoever invoked the CLI was a human, immediately labels the item `confirmed` and `user_stated`, assigns default confidence 0.9, and writes a recallable memory twin ([lessons.rs](/Users/dev/Projects/engram-cli/crates/engram-cli/src/commands/lessons.rs:300)). For a tool whose callers are AI agents, that authority assumption is false.
- `lessons retract` retires the lesson row but does not retract its recallable twin ([lessons.rs](/Users/dev/Projects/engram-cli/crates/engram-cli/src/commands/lessons.rs:549)).
- `reflect` converts edit churn and retracted extractions into procedural advice. That is exactly the self-reinforcing reasoning loop identified by the reviewers, without enough evidence to earn persistence.

Replacement: none. A user or agent can store an explicit procedural instruction using ordinary `remember`, with its actual source and without a second status system.

Axes: **safer, leaner, lighter, simpler**.

Cost: no trigger-filtered procedural listing and no automatic lesson proposals.

Migration: before dropping the table, close every recallable twin referenced by `lessons.memory_id`; otherwise the unsafe residue survives the subsystem. Record those closures in the journal.

## 5. Remove the benchmark laboratory from the shipped product

What dies:

- The top-level `bench` command.
- The 9,721-line `engram-bench` crate from the shipped workspace.
- The 1,941-line CLI benchmark dispatcher.
- Eleven product-visible suite choices and their model/download/configuration paths.

That is **11,662 production lines**, or **16.1%** of current production Rust. The current personal suite is useful as a smoke fixture but not quality evidence: it has small KBs, does not exercise facts/intervals/decisions, and excludes negative controls from headline retrieval metrics ([review-angle-retrieval.md](/Users/dev/Projects/engram-cli/docs/arch/review-angle-retrieval.md:105)).

Replacement: keep a small, version-pinned CI harness outside the installed agent binary. Preserve only safety-relevant regression cases: current-vs-retired facts, abstention negatives, forged imports, and 1k/10k/100k growth.

Axes: **leaner, lighter, simpler**, with faster builds likely but not counted because I did not rebuild a stripped binary.

Cost: developers lose convenient built-in dataset downloads and ad hoc benchmark execution. This cut must not delete the small regression gates themselves; it removes a research laboratory from the agent-facing product.

## 6. Make portable import observation-only

Use one invariant:

> Foreign bytes may create local observations; they may never import local transitions.

What dies:

- JSON replay of facts, closures, conflicts, decisions, and journal events.
- v2 genesis synthesis.
- Imported authority/origin branching and the `events.origin` column after legacy migration.
- Duplicate fact representation in both rows and events—the code itself notes that the fact row adds nothing in v3 ([export_v3.rs](/Users/dev/Projects/engram-cli/crates/engram-storage/src/export_v3.rs:201)).
- At least **367 production lines** from v2/v3 CLI dispatch and storage replay alone, before origin branches and tests.

What replaces it:

- Portable JSON interchange imports memories/documents only, through the ordinary local write path, with new local handles.
- Full-fidelity backup/restore uses SQLite’s existing `VACUUM INTO`, which correctly handles WAL stores ([backup.rs](/Users/dev/Projects/engram-cli/crates/engram-storage/src/backup.rs:1)). It restores an empty store; it is not a merge protocol.

This is simpler than repeatedly inspecting attacker-controlled `close_authority`, interval shape, and close reason. The current code documents the three forged-field passes and ultimately trusts only the local relation registry ([export_v3.rs](/Users/dev/Projects/engram-cli/crates/engram-storage/src/export_v3.rs:86)).

Axes: **safer, leaner, simpler**.

Cost: portable JSON no longer preserves event history, adjudications, embeddings, identifiers, or closed ordinary facts. It also stops pretending export/import is multi-store sync; the reviewers already demonstrated that divergent stores do not converge.

Migration: do not simply drop `origin` from existing stores. First rebuild under the present gate, flatten previously imported history into observation-only genesis records representing the safe current projection, discard foreign transition events, then remove the column. This intentionally sacrifices imported transition history.

## 7. Remove production stub embeddings

What dies:

- Persistent 64-dimensional `StubEmbedder` vectors.
- Seven `offline` CLI fields and two `allow_mixed_embeddings` escape hatches.
- Mixed-embedding guards and repair guidance.
- Silent stub fallback in ingest, visible at [ingest.rs](/Users/dev/Projects/engram-cli/crates/engram-cli/src/commands/ingest.rs:130).

What replaces it:

- Keyless/offline writes store text with `embedding = NULL`; FTS remains immediately usable.
- Rename the offline read profile to `lexical`.
- Cloud embedding is the only persistent vector space.
- Keep the 89-line deterministic stub under test/benchmark configuration only. It describes itself as “not meaningful semantically” in [stub.rs](/Users/dev/Projects/engram-cli/crates/engram-embed/src/stub.rs:1).

Measured wins:

- At least **256 BLOB bytes per offline chunk** disappear before SQLite overhead.
- One production embedding path instead of two.
- The failure described in [retrieval.rs](/Users/dev/Projects/engram-cli/crates/engram-cli/src/retrieval.rs:242)—mixed dimensions making recall fail and novelty dedup silently skip—becomes impossible.

Axes: **safer, leaner, lighter, simpler**, and faster for keyless writes.

Cost: offline semantic retrieval disappears. Given the stub’s own documented limitations, the honest replacement is lexical retrieval, not fake semantics.

Migration: null all `stub-*` embeddings and their metadata. Users wanting cloud semantics run one explicit reindex later.

## 8. Use one namespace and one verb per operation

What dies:

- The `memory add/search/update/delete/list` family.
- The second scoping dimension, `diary`.
- `memory list` moves under operator inspection or disappears.
- Six read verbs finish collapsing to `recall` and `state`.

Measured weight:

- The memory alias enum and dispatch alone are **176 lines**, before contracts/tests ([cli.rs](/Users/dev/Projects/engram-cli/crates/engram-cli/src/cli.rs:688), [lib.rs](/Users/dev/Projects/engram-cli/crates/engram-cli/src/lib.rs:341)).
- Its three independently embedded schemas occupy **8,216 minified bytes**.
- `diary` appears on seven logical tables, six indexes, and in **786 production source lines across 50 files**.
- Diary defaults are inconsistent: recall defaults to `default`, while other reads often default to all. Derived artifacts lack diary ownership, requiring the substantial isolation gate in [retrieval.rs](/Users/dev/Projects/engram-cli/crates/engram-cli/src/retrieval.rs:709).

Replacement:

- `remember`, `recall`, `edit`, and `forget` are the only memory verbs.
- KB is the sole isolation namespace.
- Migrate non-default `(kb, diary)` scopes to deterministic KB names such as `kb::diary`; preserve the old diary in source metadata.

Axes: **leaner, lighter, safer, simpler**.

Cost: users lose an orthogonal “same KB, separate agent diary” dimension and scripts using the alias family break. Cross-agent visibility becomes explicit KB selection rather than diary wildcards.

Migration: one release of deterministic error messages is enough. Do not retain aliases indefinitely.

## 9. Delete jobs and the daemon unless a real external client proves they are needed

The in-repo evidence does not justify them for CLI latency:

- Debug `--version`: 3.9 ms.
- Common metadata/list/state commands: 6–7 ms.
- Small offline recall: 9.6 ms.
- `agent-info`, the slowest measured startup command: 15.4 ms.

Full removal would delete `serve`, `engramd`, `jobs`, the scheduler, sleep compilation, Core’s daemon transport, and **5,118 production lines**. Six job schemas add another 6,830 manifest bytes.

More importantly, identical `jobs submit` calls currently mean three different things: queued with a daemon, synchronous without one, or inert when detached without one ([jobs.rs](/Users/dev/Projects/engram-cli/crates/engram-cli/src/commands/jobs.rs:1)). After deleting compile, the job registry has only fact backfill left; run that synchronously with `--limit`.

Axes: **leaner, safer, simpler**.

Cost: HTTP integrations, background resumability, writer backpressure, and the possible “one canonical remote store” deployment disappear.

Compatibility gate: search actual dependent repositories and deployed clients first. If one exists, retain a minimal synchronous HTTP adapter but still delete jobs and sleep compilation—at least 2,065 whole-file lines. If none exists, remove the entire subsystem and close outstanding jobs during migration.

## The 30% cut

Using production Rust as the denominator, 30% is 21,723 lines. Three whole-file cuts already exceed it:

- Derived compiler world: 8,935.
- Lessons/reflect: 1,877.
- Shipped benchmark laboratory: 11,662.
- Total: **22,474 lines, or 31.0%**.

That excludes shared storage/retrieval/contracts that would also disappear. Keep the safety regression cases under integration tests; do not count moved code or deleted tests as architectural savings.

## Performance finding: do not spend a release on startup

Startup is already good. The structural hot path is recall’s all-corpus dense scan: it loads every compatible embedding and content row before scoring ([retrieval.rs](/Users/dev/Projects/engram-cli/crates/engram-cli/src/retrieval.rs:732)).

With 64-dimensional stub vectors:

| Chunks | Recall | Peak RSS |
|---:|---:|---:|
| 1,000 | 16.3 ms | 23.9 MB |
| 10,000 | 87.2 ms | 39.9 MB |
| 100,000 | 865.5 ms | 196.4 MB |

That is near-linear and is only a structural lower-bound demonstration, not an estimate for 1,536-dimensional production vectors. Deleting derived candidate layers will simplify recall but will not remove this scan. A real fix needs bounded candidate retrieval or ANN indexing; pretending it can be solved by a startup refactor would violate the subtraction brief.

## Things that must not be cut

- **The journal and hot projections.** Facts, decisions, and conflict queues may be derivable, but replaying the journal for every query would be much slower. Keep the journal as audit/rebuild truth and projections as indexes.
- **Fact intervals, the safety-relation registry, and the decision ledger.** Removing origin/import machinery is safe only because portable import stops accepting transitions. Local writes still need current-versus-historic state and explicit authority.
- **`state`.** It is the only honest read surface for “true now” and as-of questions. Fix its output weight; do not replace it with raw recall.
- **Raw memories, chunks, documents, FTS, and provenance.** These are the evidence floor. The dense path needs indexing, not deletion.
- **Typed handles, structured errors, conflicts, and explicit `no_match` semantics.** These make agent behavior deterministic.
- **One untrusted-content boundary.** Delete the duplicate representation, not the warning or content/frame separation.
- **Safety regression tests.** The broad benchmark product can go; forged-import, interval, conflict, current-state, negative-retrieval, and growth tests cannot.

No repository files were modified. Prior memory was used only to route to the relevant repo truth surfaces; every finding and measurement above was rechecked against the current checkout.


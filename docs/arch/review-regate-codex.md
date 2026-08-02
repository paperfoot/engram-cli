# NO-GO

Two release blockers remain at clean HEAD `f5816a3886c2301aed75793e8508387c7f291273` (`arch/v0.4`, 76 commits over `main`).

## Blocking findings

1. **Forged POINT intervals bypass the import safety gate.**

   `close_is_point` grants the exemption solely when `valid_from == valid_until`; it verifies no cessation provenance. [export_v3.rs:128-139](/Users/dev/Projects/engram-cli/crates/engram-storage/src/export_v3.rs:128)

   All three imported-close guards skip authority validation for that shape:

   - Fact rows: [sqlite.rs:5599-5607](/Users/dev/Projects/engram-cli/crates/engram-storage/src/sqlite.rs:5599)
   - `fact.observed`: [rebuild.rs:157-169](/Users/dev/Projects/engram-cli/crates/engram-storage/src/rebuild.rs:157)
   - `fact.interval_closed`: [rebuild.rs:235-253](/Users/dev/Projects/engram-cli/crates/engram-storage/src/rebuild.rs:235)

   I attacked both row and event paths using only export → `jq` → import:

   ```text
   allergic_to | penicillin | from=T | until=T | heuristic | superseded
   ```

   The original active allergy became historic, with no rejection or queue. A forged close event can target any active safety fact and choose `valid_until` equal to its existing `valid_from`. This directly bypasses the rule that heuristics never close safety intervals. [interval.rs:154-175](/Users/dev/Projects/engram-cli/crates/engram-core/src/interval.rs:154)

   This is the requested fixes-1+2 composition regression and independently forces NO-GO. Runnable proof: [proof_point_guard.sh](/private/tmp/claude-501/-Users-biobook-Projects-engram-cli/1cbcfde2-44a2-4e6b-af05-7357a834ad31/scratchpad/regate/proof_point_guard.sh:1).

2. **Decision orientation becomes directionless again after its conflict row is purged.**

   The storage schema still has no `loser_object_norm`. [migrate_v10.rs:40-55](/Users/dev/Projects/engram-cli/crates/engram-storage/src/migrate_v10.rs:40) Consequently, every stored decision initially receives `loser_object_norm: None`, not only legacy decisions. [semantics.rs:664-672](/Users/dev/Projects/engram-cli/crates/engram-graph/src/semantics.rs:664)

   `recover_loser` depends on both the conflict row and loser fact still existing. [semantics.rs:729-749](/Users/dev/Projects/engram-cli/crates/engram-graph/src/semantics.rs:729) If either is gone, replay explicitly falls back to the old positional behavior. [semantics.rs:829-837](/Users/dev/Projects/engram-cli/crates/engram-graph/src/semantics.rs:829)

   I reached this without direct database mutation:

   1. Resolve `metformin 500 mg` versus `1000 mg` with `supersede-old`; caller keeps `1000 mg`.
   2. `kb delete medical --confirm` cascades the conflict row through the fact FKs, while the decision survives. [sqlite.rs:1469-1479](/Users/dev/Projects/engram-cli/crates/engram-storage/src/sqlite.rs:1469), [sqlite.rs:1260-1262](/Users/dev/Projects/engram-cli/crates/engram-storage/src/sqlite.rs:1260)
   3. Recreate `medical` and derive the pair in reverse order.

   Result:

   ```text
   metformin 1000 mg | closed | replayed_caller | replayed_close_old
   metformin 500 mg  | open
   ```

   The caller’s decision was reversed on a safety relation, silently and without queueing. Runnable proof: [proof_replay_after_purge.sh](/private/tmp/claude-501/-Users-biobook-Projects-engram-cli/1cbcfde2-44a2-4e6b-af05-7357a834ad31/scratchpad/regate/proof_replay_after_purge.sh:1).

## Re-gate matrix

| Item | Result |
|---|---|
| 1. Extractor forging `ExplicitEnd` | **Closed.** Original stub attack left metformin active, recorded the hallucinated ending as a heuristic point, and queued `requires_context`. [semantics.rs:960-967](/Users/dev/Projects/engram-cli/crates/engram-graph/src/semantics.rs:960), [semantics.rs:1208-1288](/Users/dev/Projects/engram-cli/crates/engram-graph/src/semantics.rs:1208) |
| 2. Tampered v3 import | **Not fully closed.** Ordinary heuristic span imports open and inverted intervals are rejected, but POINT rows/events bypass the same gate. |
| 3. Directionless replay | **Not fully closed.** Reversal works while the conflict row survives; the production purge path above reverses it again. |
| 4. Orphan conflict deletion | **Closed.** Rebuild skips deleting orphans. [rebuild.rs:594-614](/Users/dev/Projects/engram-cli/crates/engram-storage/src/rebuild.rs:594) The 15,458-row reproduction retained both orphans and had an identical inclusive digest. |
| 5. Confirmation reset | **Closed.** `fact.observed` carries and restores confirmations. [journal.rs:455-461](/Users/dev/Projects/engram-cli/crates/engram-storage/src/journal.rs:455), [rebuild.rs:205-214](/Users/dev/Projects/engram-cli/crates/engram-storage/src/rebuild.rs:205) The realistic fixture remained at 7. |
| 6. Unwelded extraction marker | **Closed.** Production calls `record_derivation`, which writes the event and both markers transactionally. [fact_write.rs:614-633](/Users/dev/Projects/engram-cli/crates/engram-cli/src/fact_write.rs:614), [write_path.rs:255-327](/Users/dev/Projects/engram-cli/crates/engram-storage/src/write_path.rs:255) Marker/derivation/event counts stayed `1/1/1`; backfill stayed `pending: 0`. |
| 7. Undeclared wire errors | **Closed for the known changes.** All three commands return exit 3 `not_found` and the Breaking bullet names each. [CHANGELOG.md:23-29](/Users/dev/Projects/engram-cli/CHANGELOG.md:23) |
| Removed `resolve_fact_conflict` | **Closed.** Only explanatory comments remain; the welded resolve/rebuild test passes. [sqlite.rs:4999](/Users/dev/Projects/engram-cli/crates/engram-storage/src/sqlite.rs:4999), [decisions.rs:708](/Users/dev/Projects/engram-cli/crates/engram-storage/src/decisions.rs:708) |
| Version advisory | **Closed.** Binary, manifest, Cargo, CHANGELOG and README report `0.4.0`. [Cargo.toml:16-34](/Users/dev/Projects/engram-cli/Cargo.toml:16), [README.md:173](/Users/dev/Projects/engram-cli/README.md:173) |
| Schema advisory | **Closed.** Every registered schema path executes successfully. [contract_schema.rs:26-57](/Users/dev/Projects/engram-cli/crates/engram-cli/tests/contract_schema.rs:26) |

The realistic v9 run completed with `1008` memories, `311` facts, `15458` conflicts, `14504` decisions, `31286` events, confirmation count `7`, and `integrity_check=ok`.

## Remaining advisories

- **Medium — the new rejection/preservation counters are not exposed publicly.** `ImportV3Report.close_authority_rejected` is accumulated but omitted from the CLI response. [export_v3.rs:78-81](/Users/dev/Projects/engram-cli/crates/engram-storage/src/export_v3.rs:78), [import.rs:90-118](/Users/dev/Projects/engram-cli/crates/engram-cli/src/commands/import.rs:90) Likewise, rebuild computes `conflicts_preserved_orphan`, but `admin rebuild` omits it and still says the rows “could not be reinserted.” [rebuild.rs:600-607](/Users/dev/Projects/engram-cli/crates/engram-storage/src/rebuild.rs:600), [admin.rs:42-80](/Users/dev/Projects/engram-cli/crates/engram-cli/src/commands/admin.rs:42)

- **Low — compatibility completeness remains manually curated.** `contract_breaking.rs` validates only entries already placed in `CHANGED_WIRE_ERRORS`; it does not compare behavior against `main`, so an unlisted future break remains invisible. [contract_breaking.rs:35-66](/Users/dev/Projects/engram-cli/crates/engram-cli/tests/contract_breaking.rs:35)

- **Low — inverted import gives wrong remediation.** It exits `malformed_json` and tells the caller the store is corrupt and to run `doctor`, although the bad value came from the import file. [error.rs:111](/Users/dev/Projects/engram-cli/crates/engram-cli/src/error.rs:111), [error.rs:249-250](/Users/dev/Projects/engram-cli/crates/engram-cli/src/error.rs:249)

Current HEAD enumerates 1,163 tests. All 1,066 non-listener tests available here passed; the full run stopped at the first listener-dependent test because this sandbox rejects `TcpListener::bind` with `PermissionDenied`. [daemon_support/mod.rs:108](/Users/dev/Projects/engram-cli/crates/engram-cli/tests/daemon_support/mod.rs:108) The evidence gate failed on the two product attacks above; the third open proof is only that sandbox limitation.


NO-GO

Seven release blockers are reproducible.

## Blockers

1. **The extraction model can forge `ExplicitEnd` authority and close a safety interval.**

   `ExtractedFact::says_ended()` trusts the model’s `state` token; `build_observation` promotes it to `ObservationState::Ended` without checking `SourceKind`, and cessation planning then assigns `CloseAuthority::ExplicitEnd`. [facts.rs:117](/Users/dev/Projects/engram-cli/crates/engram-graph/src/facts.rs:117), [fact_write.rs:650](/Users/dev/Projects/engram-cli/crates/engram-cli/src/fact_write.rs:650), [semantics.rs:1044](/Users/dev/Projects/engram-cli/crates/engram-graph/src/semantics.rs:1044). This defeats the stated person-supplied-authority rule in [interval.rs:154](/Users/dev/Projects/engram-cli/crates/engram-core/src/interval.rs:154).

   Exact reproduction:

   ```sh
   engram --store S remember 'Ana is taking metformin.' --kb personal --offline \
     --facts-json '[{"subject":"Ana","predicate":"takes_medication","object":"metformin","state":"ongoing"}]'

   ENGRAM_FACT_LLM_STUB='{"*":[{"subject":"Ana","predicate":"takes_medication","object":"metformin","state":"ended"}]}' \
     engram --store S remember 'Ana confirms that she is still taking metformin.' \
     --kb personal --offline
   ```

   The second write reports `facts_source_kind:"agent_extracted"` but closes metformin with `close_reason:"stopped"` and `close_authority:"explicit_end"`, even though the source says the opposite. A real provider returning the same hallucinated token reaches the identical path.

   Tests miss it because interval tests use caller-supplied `--facts-json` whose text genuinely says “stopped,” while semantics tests manually set `ObservationState::Ended` and assume that means “the TEXT speaking.” They never test contradictory model output or require `UserStated` provenance. [wave3_fact_intervals.rs:13](/Users/dev/Projects/engram-cli/crates/engram-cli/tests/wave3_fact_intervals.rs:13), [semantics/tests.rs:507](/Users/dev/Projects/engram-cli/crates/engram-graph/src/semantics/tests.rs:507).

2. **A tampered v3 import bypasses `may_close` entirely.**

   V3 fact rows expose `valid_until`, `close_reason`, and `close_authority`; import writes those columns directly before replaying events. Neither row import nor event replay calls `may_close`. [sqlite.rs:329](/Users/dev/Projects/engram-cli/crates/engram-storage/src/sqlite.rs:329), [export_v3.rs:147](/Users/dev/Projects/engram-cli/crates/engram-storage/src/export_v3.rs:147), [sqlite.rs:5578](/Users/dev/Projects/engram-cli/crates/engram-storage/src/sqlite.rs:5578), [rebuild.rs:144](/Users/dev/Projects/engram-cli/crates/engram-storage/src/rebuild.rs:144).

   Exact reproduction:

   ```sh
   engram --store /tmp/source remember 'Ana is allergic to penicillin.' --offline \
     --facts-json '[{"subject":"Ana","predicate":"allergic_to","object":"penicillin"}]'
   engram --store /tmp/source export --json > export.json

   jq '
     .data.events=[] |
     .data.counts.events=0 |
     .data.facts[0].valid_until="2026-08-01T00:00:00Z" |
     .data.facts[0].close_reason="superseded" |
     .data.facts[0].close_authority="heuristic"
   ' export.json > forged.json

   engram --store /tmp/target import forged.json --json
   ```

   Import succeeds and creates a closed `allergic_to` interval with `close_authority:"heuristic"`. It also accepts `valid_until < valid_from`. This is distinct from accepted migrated-v9 heuristic history: it is a fresh, caller-editable import forging prohibited authority.

   Tests only round-trip self-generated exports, repeat the same file, test v2 genesis, and reject future schema versions. There is no tamper, authority, interval-order, count, or event-completeness test. [export_v3.rs:347](/Users/dev/Projects/engram-cli/crates/engram-storage/src/export_v3.rs:347).

3. **Directionless semantic replay reverses a caller’s decision when pair order reverses.**

   The semantic key deliberately sorts the object pair; `DecisionRecord` stores no original winner/loser orientation. Replay therefore maps `CloseOld` to the current pair’s “old” row, not the fact the caller originally rejected. [keys.rs:26](/Users/dev/Projects/engram-cli/crates/engram-core/src/keys.rs:26), [semantics.rs:524](/Users/dev/Projects/engram-cli/crates/engram-graph/src/semantics.rs:524), [semantics.rs:622](/Users/dev/Projects/engram-cli/crates/engram-graph/src/semantics.rs:622), [semantics.rs:699](/Users/dev/Projects/engram-cli/crates/engram-graph/src/semantics.rs:699), [semantics.rs:1110](/Users/dev/Projects/engram-cli/crates/engram-graph/src/semantics.rs:1110).

   Trigger:

   ```sh
   engram remember 'Carol works at Acme and Beta Corp.' --kb personal --offline \
     --facts-json '[{"subject":"Carol","predicate":"works_at","object":"Acme"},{"subject":"Carol","predicate":"works_at","object":"Beta Corp"}]'
   engram facts resolve <returned-conflict-id> --action supersede-old
   engram remember 'Reversed re-derivation: Beta Corp and Acme.' --kb personal --offline \
     --facts-json '[{"subject":"Carol","predicate":"works_at","object":"Beta Corp"},{"subject":"Carol","predicate":"works_at","object":"Acme"}]'
   ```

   The first adjudication closes Acme and keeps Beta. The second write silently reports `replayed_close_old`, closes Beta with `replayed_caller`, and leaves Acme open—the opposite decision. On a safety dose pair, that replay authority passes the safety gate.

   Tests separately prove pair-order-independent keys and same-orientation replay, but never reverse the roles during re-derivation. [semantics/tests.rs:774](/Users/dev/Projects/engram-cli/crates/engram-graph/src/semantics/tests.rs:774), [semantics/tests.rs:865](/Users/dev/Projects/engram-cli/crates/engram-graph/src/semantics/tests.rs:865), [wave3_fact_replay.rs:128](/Users/dev/Projects/engram-cli/crates/engram-cli/tests/wave3_fact_replay.rs:128).

4. **`admin rebuild` deletes migrated orphan conflict history.**

   Migration synthesizes `conflict.opened` for orphan rows, but its orphan query lacks a status predicate and includes both open and resolved rows. Rebuild first deletes the entire queue, then refuses to recreate an orphan and merely increments `conflicts_skipped_orphan`. [migrate_v10.rs:727](/Users/dev/Projects/engram-cli/crates/engram-storage/src/migrate_v10.rs:727), [rebuild.rs:225](/Users/dev/Projects/engram-cli/crates/engram-storage/src/rebuild.rs:225), [rebuild.rs:479](/Users/dev/Projects/engram-cli/crates/engram-storage/src/rebuild.rs:479), [rebuild.rs:538](/Users/dev/Projects/engram-cli/crates/engram-storage/src/rebuild.rs:538).

   Trigger state: migrate a v9 store containing the fixture’s open and resolved orphan rows, then run:

   ```sh
   engram admin rebuild --confirm --json
   ```

   On the 15,458-row realistic queue, rebuild changed the queue to 15,456 and removed both orphan IDs, reporting `conflicts_skipped_orphan:2`. This contradicts the unqualified byte-identical promise in [CHANGELOG.md:57](/Users/dev/Projects/engram-cli/CHANGELOG.md:57).

   Tests miss it deliberately: the digest excludes rows whose facts are absent and then asserts that skipping two orphans is acceptable. [migration_v10.rs:531](/Users/dev/Projects/engram-cli/crates/engram-storage/tests/migration_v10.rs:531), [migration_v10.rs:584](/Users/dev/Projects/engram-cli/crates/engram-storage/tests/migration_v10.rs:584). The fixture explicitly contains both orphan shapes at [v9_store.sql:401](/Users/dev/Projects/engram-cli/crates/engram-storage/tests/fixtures/v9_store.sql:401).

5. **`admin rebuild` resets historical confirmation counts.**

   Live insertion stores `Fact.confirmations`, but `fact.observed` omits it. Replay always inserts `confirmations=0`, while migration emits no synthetic `fact.confirmed` events for the existing count. [sqlite.rs:4263](/Users/dev/Projects/engram-cli/crates/engram-storage/src/sqlite.rs:4263), [journal.rs:429](/Users/dev/Projects/engram-cli/crates/engram-storage/src/journal.rs:429), [migrate_v10.rs:420](/Users/dev/Projects/engram-cli/crates/engram-storage/src/migrate_v10.rs:420), [rebuild.rs:103](/Users/dev/Projects/engram-cli/crates/engram-storage/src/rebuild.rs:103).

   Trigger state: a legitimate v9 fact with `confirmations=7`, migration to v11, then:

   ```sh
   engram admin rebuild --confirm --json
   ```

   The migrated row remained at 7 before rebuild and became 0 afterwards.

   The migration digest does include confirmations, but every fixture fact has zero confirmations, so the omission is invisible. [migration_v10.rs:537](/Users/dev/Projects/engram-cli/crates/engram-storage/tests/migration_v10.rs:537), [v9_store.sql:169](/Users/dev/Projects/engram-cli/crates/engram-storage/tests/fixtures/v9_store.sql:169). The CLI rebuild digest omits the field entirely. [wave2_admin_rebuild.rs:59](/Users/dev/Projects/engram-cli/crates/engram-cli/tests/wave2_admin_rebuild.rs:59).

6. **Normal extraction writes its processed marker without the event required to rebuild it.**

   `write_fact_batch` directly calls `record_fact_extraction`, which mutates `fact_extraction_log` without an event. The correctly welded `record_derivation` method emits `derivation.ran`, but production extraction does not call it. Rebuild deletes both derivation tables and can restore markers only from `derivation.ran`. [fact_write.rs:571](/Users/dev/Projects/engram-cli/crates/engram-cli/src/fact_write.rs:571), [sqlite.rs:2257](/Users/dev/Projects/engram-cli/crates/engram-storage/src/sqlite.rs:2257), [write_path.rs:255](/Users/dev/Projects/engram-cli/crates/engram-storage/src/write_path.rs:255), [rebuild.rs:175](/Users/dev/Projects/engram-cli/crates/engram-storage/src/rebuild.rs:175), [rebuild.rs:492](/Users/dev/Projects/engram-cli/crates/engram-storage/src/rebuild.rs:492).

   Exact reproduction:

   ```sh
   ENGRAM_FACT_LLM_STUB='{"*":[]}' engram remember \
     'No extractable atomic claim here.' --kb personal --offline
   engram facts backfill --kb personal --dry-run
   engram admin rebuild --confirm
   engram facts backfill --kb personal --dry-run
   ```

   Before rebuild: marker count 1 and `pending:0`. After rebuild: marker count 0 and `pending:1`. A non-dry backfill now repeats a provider call the store had already completed.

   Tests miss it because rebuild seeding uses `--facts-json`, while derivation tests call `record_derivation` directly rather than exercising normal `remember → rebuild`. [wave2_admin_rebuild.rs:26](/Users/dev/Projects/engram-cli/crates/engram-cli/tests/wave2_admin_rebuild.rs:26), [write_path.rs:721](/Users/dev/Projects/engram-cli/crates/engram-storage/src/write_path.rs:721).

7. **The breaking-change list omits existing wire-error changes.**

   Against the same absent UUID:

   | Invocation | `main` | `arch/v0.4` |
   |---|---:|---:|
   | `edit UUID --importance 3 --json` | exit 3, `bad_input` | exit 3, `not_found` |
   | `facts resolve UUID --action keep-both --json` | exit 3, `bad_input` | exit 3, `not_found` |
   | `jobs show UUID --json` | exit 3, `bad_input` | exit 3, `not_found` |

   Main behavior is at `main:crates/engram-cli/src/commands/edit.rs:60-63`, `main:.../facts.rs:459-460`, and `main:.../jobs.rs:30-31`. Branch behavior comes from [memory.rs:123](/Users/dev/Projects/engram-cli/crates/engram-cli/src/commands/memory.rs:123), [resolve.rs:68](/Users/dev/Projects/engram-cli/crates/engram-cli/src/engine/resolve.rs:68), and [jobs.rs:56](/Users/dev/Projects/engram-cli/crates/engram-cli/src/commands/jobs.rs:56).

   `error.code` is explicitly treated as a frozen wire value in [error.rs:168](/Users/dev/Projects/engram-cli/crates/engram-cli/src/error.rs:168). The breaking section declares only the unknown-`forget` and Clap parse changes, not these three existing invocations. [CHANGELOG.md:11](/Users/dev/Projects/engram-cli/CHANGELOG.md:11).

   Tests miss completeness because they pin the new taxonomy against the current registry/current binary; they never compare behavior with `main` or require each changed wire outcome to appear under “Breaking.” [contract_surface.rs:182](/Users/dev/Projects/engram-cli/crates/engram-cli/tests/contract_surface.rs:182), [contract_manifest.rs:94](/Users/dev/Projects/engram-cli/crates/engram-cli/tests/contract_manifest.rs:94).

## Advisories

- **High — the release still identifies and packages itself as 0.3.1.** Workspace and internal dependency versions remain `0.3.1`; `engram --version` and `agent-info.version` both return `0.3.1`. [Cargo.toml:16](/Users/dev/Projects/engram-cli/Cargo.toml:16). The version test checks only successful execution and store isolation, not the expected release value. [contract_discovery.rs:25](/Users/dev/Projects/engram-cli/crates/engram-cli/tests/contract_discovery.rs:25).

- **High — the advertised JSON-Schema discovery command does not exist.** `AgentInfo` is a unit variant, yet parser and runtime errors repeatedly instruct agents to run `engram agent-info --schema '<command>'`. [cli.rs:30](/Users/dev/Projects/engram-cli/crates/engram-cli/src/cli.rs:30), [cli.rs:1285](/Users/dev/Projects/engram-cli/crates/engram-cli/src/cli.rs:1285), [error.rs:195](/Users/dev/Projects/engram-cli/crates/engram-cli/src/error.rs:195). That invocation exits 3 with `bad_input` and repeats the same impossible suggestion. The design explicitly expected a schema test for every command. [spec-contract-codegen.md:633](/Users/dev/Projects/engram-cli/docs/arch/spec-contract-codegen.md:633), [spec-contract-codegen.md:1177](/Users/dev/Projects/engram-cli/docs/arch/spec-contract-codegen.md:1177). The tests do launch the real binary, but mostly compare the binary manifest with another rendering from the same registry or grep `<command> --help`; they do not execute the 36 schema paths or most command behaviors. [contract_support/mod.rs:29](/Users/dev/Projects/engram-cli/crates/engram-cli/tests/contract_support/mod.rs:29), [contract_manifest.rs:13](/Users/dev/Projects/engram-cli/crates/engram-cli/tests/contract_manifest.rs:13), [contract_surface.rs:50](/Users/dev/Projects/engram-cli/crates/engram-cli/tests/contract_surface.rs:50).

Migration and concurrency checks that held:

- A schema-v9 fixture with 1,008 memories, 1,008 chunks, 311 facts, three KBs, and 15,458 queue rows migrated to v11 in 1.87 seconds, producing 31,286 genesis events and 14,504 decisions. The backup path was recorded.
- Killing the process 0.5 seconds into that migration left schema 9, no `events` table, all 15,458 queue rows, and `integrity_check=ok`. Re-running reached v11 and created a collision-safe second backup. Backup-before-transaction and transactional migration are correctly implemented at [sqlite.rs:929](/Users/dev/Projects/engram-cli/crates/engram-storage/src/sqlite.rs:929), [sqlite.rs:1006](/Users/dev/Projects/engram-cli/crates/engram-storage/src/sqlite.rs:1006), and [backup.rs:46](/Users/dev/Projects/engram-cli/crates/engram-storage/src/backup.rs:46).
- I found no additional lost-write path in the reviewed CLI/Core interleavings. The direct crossed-action, two-process resolve race produced exactly one winner. [daemon_resolve_race.rs:58](/Users/dev/Projects/engram-cli/crates/engram-cli/tests/daemon_resolve_race.rs:58). The Core serializes each task inside `with_tx_res`, and independent connections use a five-second busy timeout. [engine/mod.rs:201](/Users/dev/Projects/engram-cli/crates/engram-cli/src/engine/mod.rs:201), [sqlite.rs:921](/Users/dev/Projects/engram-cli/crates/engram-storage/src/sqlite.rs:921).
- 412 focused core/storage/graph/contract/rebuild/replay/write-surface/race tests passed, and the evidence gate passed all seven audit obligations. Full listener-dependent daemon execution was unavailable because this sandbox forbids the test’s `TcpListener::bind`. [daemon_support/mod.rs:104](/Users/dev/Projects/engram-cli/crates/engram-cli/tests/daemon_support/mod.rs:104).
- The audited `arch/v0.4` worktree remains clean.


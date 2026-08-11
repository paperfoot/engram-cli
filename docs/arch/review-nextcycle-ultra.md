NO-GO as ordered. Three parallel lanes read every assigned document through EOF; I separately read all five B/C code files in full. The checkout is clean at `arch/v0.4` / `d3ae33a`. I did not open the live store or run a reproduction—the decisive claims are provable directly from code.

## High

1. **B × C can silently resurrect a superseded assertion.**

   B attaches correction to one physical memory row by setting `valid_until`; C replaces a changed document by generating a fresh, unexpired memory ID. If source text X is superseded by Y, then an unrelated edit to X’s file followed by sync makes X current again under a new ID. C7’s “old chunks gone, new chunks found” test would still pass.

   This repeats the representation-regeneration class from the expiry gates: safety state attached to one representation disappears when the representation is rebuilt ([next-cycle-plan.md:28–37](/Users/dev/Projects/engram-cli/docs/arch/next-cycle-plan.md:28), [next-cycle-plan.md:81–92](/Users/dev/Projects/engram-cli/docs/arch/next-cycle-plan.md:81), [ingest.rs:232–248](/Users/dev/Projects/engram-cli/crates/engram-cli/src/commands/ingest.rs:232), [ingest.rs:282–314](/Users/dev/Projects/engram-cli/crates/engram-cli/src/commands/ingest.rs:282), [gate 1:5–10](/private/tmp/claude-501/-Users-biobook-Projects-engram-cli/d3dc346f-f0cd-4a0f-8741-851c303900d0/scratchpad/codex-review-out.md:5)).

   Before either mutating feature, rule whether the file or supersession is authoritative. If supersession wins, row-local `valid_until` plus an informational journal event is insufficient; re-ingest needs a durable logical suppression/lineage rule. Repeat the scenario for `forget`.

2. **The execution order violates the inherited gate and points subtraction at now-load-bearing code.**

   Fact-extraction inversion is explicitly Tier-2 priority 1, yet D comes after B/C and contains design only ([field-report-deltas.md:93–98](/Users/dev/Projects/engram-cli/docs/arch/field-report-deltas.md:93), [HANDOFF.md:102–108](/Users/dev/Projects/engram-cli/docs/arch/HANDOFF.md:102), [next-cycle-plan.md:95–122](/Users/dev/Projects/engram-cli/docs/arch/next-cycle-plan.md:95)). More seriously, E3 says A5+D8 can arm subtraction, although gate 4 requires authority/receipts, response v2, fact-owned identity, retained commands/people, export compatibility, the P4 inventory, preserved calibration evidence, and old-store/journal/export migration proof ([next-cycle-plan.md:126–132](/Users/dev/Projects/engram-cli/docs/arch/next-cycle-plan.md:126), [review-plangate4-codex.md:378–404](/Users/dev/Projects/engram-cli/docs/arch/review-plangate4-codex.md:378)).

   The subtraction plan itself is stale: it deletes `memory.valid_until` because it supposedly has no close path, while 0.4.1 and B make it load-bearing; it also deletes `MemoryCommand::Add` while B adds a new flag there ([subtraction-plan.md:72–97](/Users/dev/Projects/engram-cli/docs/arch/subtraction-plan.md:72), [subtraction-plan.md:195–221](/Users/dev/Projects/engram-cli/docs/arch/subtraction-plan.md:195), [next-cycle-plan.md:33–39](/Users/dev/Projects/engram-cli/docs/arch/next-cycle-plan.md:33)).

   Rebase the subtraction plan on 0.4.1 before B. Retain `valid_until`; implement B only on canonical `remember`; replace E3 with the complete gate-4 checklist.

3. **“Closing the old memory IS the demotion” is false across structured reads.**

   `set_memory_valid_until` updates only the memory row and journals metadata ([sqlite.rs:2299–2349](/Users/dev/Projects/engram-cli/crates/engram-storage/src/sqlite.rs:2299)). Facts derived from that memory remain open because their predicate is based on the fact’s own `valid_until` and `superseded_by` ([sqlite.rs:879–886](/Users/dev/Projects/engram-cli/crates/engram-storage/src/sqlite.rs:879), [sqlite.rs:4596–4647](/Users/dev/Projects/engram-cli/crates/engram-storage/src/sqlite.rs:4596)). `remember` persists those fact batches separately after the memory/chunks commit ([remember.rs:504–637](/Users/dev/Projects/engram-cli/crates/engram-cli/src/commands/remember.rs:504)).

   Consequently, recall can hide X while `state`, facts and a future fact-backed `people` view continue asserting X. Closing those facts would be an authority act, so B prejudges D1. Expanded D must first rule whether supersession is memory-only or closes/replaces source facts, then test every structured read surface.

4. **B4 is neither atomic nor durably recoverable, and its idempotency criterion is currently impossible.**

   The plan deliberately commits the successor, then closes the predecessor, then appends the link; its only recovery artifact is a response that a crash may prevent ([next-cycle-plan.md:44–48](/Users/dev/Projects/engram-cli/docs/arch/next-cycle-plan.md:44)). Yet storage already supports joining all writes into one rollback-capable transaction ([sqlite.rs:1021–1047](/Users/dev/Projects/engram-cli/crates/engram-storage/src/sqlite.rs:1021)).

   `set_memory_valid_until` is a no-op only for the exact same instant; a retry’s new `now` changes the row and emits another event ([sqlite.rs:2323–2348](/Users/dev/Projects/engram-cli/crates/engram-storage/src/sqlite.rs:2323)). Journal appends mint random event IDs ([journal.rs:289–305](/Users/dev/Projects/engram-cli/crates/engram-storage/src/journal.rs:289)). Therefore “double-supersede is idempotent” is unverifiable without an operation ID, stable boundary and same-pair versus competing-successor rule.

   Make successor creation/dedup resolution, monotonic predecessor close and one typed link receipt atomic. Otherwise introduce a durable pending-operation ledger and deterministic repair command—contradicting B1’s “no table/column.”

5. **C’s claimed automatic retraction quarantine is not the general invariant it assumes.**

   Ordinary ingested memories have `valid_until = NULL` ([ingest.rs:232–248](/Users/dev/Projects/engram-cli/crates/engram-cli/src/commands/ingest.rs:232)). Document deletion closes source facts and removes source claims, but leaves wiki/entity/relation aggregates until compile ([sqlite.rs:2144–2207](/Users/dev/Projects/engram-cli/crates/engram-storage/src/sqlite.rs:2144), [sqlite.rs:3002–3040](/Users/dev/Projects/engram-cli/crates/engram-storage/src/sqlite.rs:3002)). The new KB-level retraction quarantine requires the memory to be both deleted **and already expired** ([sqlite.rs:4256–4286](/Users/dev/Projects/engram-cli/crates/engram-storage/src/sqlite.rs:4256)); aggregate retrieval gates on that set ([retrieval.rs:1021–1054](/Users/dev/Projects/engram-cli/crates/engram-cli/src/retrieval.rs:1021), [retrieval.rs:1510–1535](/Users/dev/Projects/engram-cli/crates/engram-cli/src/retrieval.rs:1510)). The separate deletion gate protects cross-diary privacy, not same-diary or unscoped freshness.

   Thus C1/C7’s blanket acceptance claim is false ([next-cycle-plan.md:68–72](/Users/dev/Projects/engram-cli/docs/arch/next-cycle-plan.md:68), [next-cycle-plan.md:88–91](/Users/dev/Projects/engram-cli/docs/arch/next-cycle-plan.md:88)). Either add a general deleted-source aggregate quarantine, require immediate successful compile, or—preferably—ship status now and defer destructive sync/prune until subtraction removes those aggregates.

6. **C is not yet a complete or safe reconciliation algorithm.**

   Current ingest stores `file.display()`—not necessarily an absolute path—and storage identity is exact `(kb, source_path)` ([ingest.rs:186–201](/Users/dev/Projects/engram-cli/crates/engram-cli/src/commands/ingest.rs:186), [sqlite.rs:2627–2640](/Users/dev/Projects/engram-cli/crates/engram-storage/src/sqlite.rs:2627)). The plan defines no sync-root ownership, diary dimension, normalization, symlink policy, nested-root behavior or legacy-relative-path migration.

   It also has:

   - No new-file/add branch, so its stated move = delete+add rule cannot execute.
   - No safe adoption path for the entire v13 corpus: NULL means unknown and unknown is excluded forever.
   - No complete-scan requirement, so missing/unmounted/unreadable roots can become mass prune.
   - No bulk confirmation or maximum-prune guard, despite single-document deletion requiring `--confirm` ([documents.rs:108–115](/Users/dev/Projects/engram-cli/crates/engram-cli/src/commands/documents.rs:108)).
   - No failed/partial/added/missing report fields.

   Changed same-path replacement already exists and is atomic per file ([ingest.rs:186–201](/Users/dev/Projects/engram-cli/crates/engram-cli/src/commands/ingest.rs:186), [ingest.rs:282–314](/Users/dev/Projects/engram-cli/crates/engram-cli/src/commands/ingest.rs:282)). C should extract and reuse that primitive, not compose public delete/re-ingest verbs into an underspecified saga.

## Medium

1. **C2/C3 omit the migration, portability and replay contract.**

   The store is v13, `DocumentRecord` and export rows have no hash fields, and export/import explicitly enumerate the old shape ([sqlite.rs:877](/Users/dev/Projects/engram-cli/crates/engram-storage/src/sqlite.rs:877), [sqlite.rs:98–109](/Users/dev/Projects/engram-cli/crates/engram-storage/src/sqlite.rs:98), [sqlite.rs:270–282](/Users/dev/Projects/engram-cli/crates/engram-storage/src/sqlite.rs:270), [sqlite.rs:5601–5626](/Users/dev/Projects/engram-cli/crates/engram-storage/src/sqlite.rs:5601), [sqlite.rs:5745–5765](/Users/dev/Projects/engram-cli/crates/engram-storage/src/sqlite.rs:5745)). Specify v14, downgrade behavior, export/import/rebuild/API parity and legacy adoption. Persist an ingest recipe/pipeline fingerprint too: equal bytes are not equivalent after D changes extraction policy.

2. **B’s scope and target-state matrix is missing.**

   Cross-KB is allowed with only a warning; cross-diary is silent. Per-KB export filters events by the event’s one KB, so a journal-only cross-KB relation can be absent from one exported side ([next-cycle-plan.md:33–37](/Users/dev/Projects/engram-cli/docs/arch/next-cycle-plan.md:33), [export_v3.rs:145–169](/Users/dev/Projects/engram-cli/crates/engram-storage/src/export_v3.rs:145)). Dedup also considers expired memories eligible ([sqlite.rs:1730–1749](/Users/dev/Projects/engram-cli/crates/engram-storage/src/sqlite.rs:1730)).

   Default to same diary and same KB; reject cross-diary before writing. Define already expired, future-dated, retracted, dedup-to-target, dedup-to-expired, same-pair retry, different successor, and forget-successor behavior. Supersession must never extend an earlier expiry.

3. **A’s evidence gate is not measurable enough and A5 is secretly three steps.**

   One day-3 jobs snapshot cannot prove drainage, “FEELS wrong” has no disposition rule, and zero integrity incidents does not cover recall defects or queue growth ([next-cycle-plan.md:13–24](/Users/dev/Projects/engram-cli/docs/arch/next-cycle-plan.md:13)). Split baseline/sampling, human gate decision, HANDOFF update and backup retention. Keep a fresh, restore-verified v13 backup; backup deletion must remain an explicit operator-only action.

4. **The response and acceptance criteria are not stable enough to gate.**

   B introduces duplicated `{id, handle}` and an ad hoc response-only partial state while response v2 is unresolved; C’s report has no errors or operation receipt ([next-cycle-plan.md:44–50](/Users/dev/Projects/engram-cli/docs/arch/next-cycle-plan.md:44), [next-cycle-plan.md:85–92](/Users/dev/Projects/engram-cli/docs/arch/next-cycle-plan.md:85), [next-cycle-plan.md:137–142](/Users/dev/Projects/engram-cli/docs/arch/next-cycle-plan.md:137)). Ratify a typed operation receipt before B/C. Replace the SQLite-file-hash dry-run canary with table/event/count equality.

   Score provenance is different: B/C need no ranking threshold, so keeping calibration after subtraction is sound.

5. **The plan overstates completion and understates effort.**

   “Tier 1 … is done” contradicts the source tier, which still includes the deferred single-copy wire and abstention floor ([next-cycle-plan.md:3–6](/Users/dev/Projects/engram-cli/docs/arch/next-cycle-plan.md:3), [field-report-deltas.md:81–91](/Users/dev/Projects/engram-cli/docs/arch/field-report-deltas.md:81)). B is not a 0.5–1-day change once atomicity, facts, receipts, concurrency and gates are included; C is not 2–3 days once migration, adoption, ownership, recovery and portability are included.

## Low

1. `synced_at` has no writer, consumer or trust semantics; remove it or define it as the atomic commit time for a particular hash ([next-cycle-plan.md:73–80](/Users/dev/Projects/engram-cli/docs/arch/next-cycle-plan.md:73)).

2. Track B omits the adopted contradiction-triggered supersession proposal; add it or explicitly defer it ([field-report-deltas.md:43–46](/Users/dev/Projects/engram-cli/docs/arch/field-report-deltas.md:43), [next-cycle-plan.md:50–56](/Users/dev/Projects/engram-cli/docs/arch/next-cycle-plan.md:50)).

3. Release boundaries are ambiguous: B is 0.4.2, C-status “ships immediately,” and full C is 0.4.2 or 0.4.3. Each schema/release boundary needs its own contract, migration and install gate.

## Per-track disposition

| Track | Disposition |
|---|---|
| A | Start now in operator-owned calendar time. It gates subtraction only. Add baselines and split backup cleanup from the architectural decision. |
| B | Keep before **mutating** C, but only after expanded D. Canonical `remember` only; atomic operation, durable relation, explicit facts/scope/chain semantics, independent gate. |
| C | Identity/bootstrap/status can proceed before or parallel with B after its rulings. Destructive sync/prune should follow B and preferably subtraction. |
| D | Move first. Expand it to cover supersession authority/fact effects, ingest/sync parity, typed receipts, response-v2 policy, fact-owned identity and export compatibility. |
| E | Decide E1 before arming subtraction. E2 needs a risk disposition. E3 must become the complete gate-4 checklist. E4 is directionally correct. |

## Missed details

- Logical correction identity surviving re-ingestion.
- Supersede/forget/prune chains and a no-implicit-revival rule.
- Same successor retry versus competing successors.
- Monotonic closure for already-expired and future-dated targets.
- Dedup-to-target and dedup-to-expired behavior.
- Same-diary/same-KB mutation authorization and cross-KB export.
- Legacy NULL-hash adoption without blessing today’s stale bytes.
- New and moved file discovery.
- Sync-root ownership and separately ingested files under that root.
- Root-unavailable, partial traversal, unreadable file and symlink-escape states.
- Hashing and parsing the same immutable byte snapshot.
- Pipeline/parser/chunker/extractor fingerprinting.
- Concurrent supersede × sync and sync × sync CAS behavior.
- Crash/fault injection at every durable boundary.
- Generic deleted-source aggregate quarantine versus expiry-only quarantine.
- v14/export/import/rebuild/daemon compatibility.
- Preserving the calibration/safety harness before removing `engram-bench`.
- A restore-tested v13 backup after old backups are removed.

## E-track recommendations

- **Thin people view:** directionally sound, but require a representative fact-coverage audit and fact-owned identity ruling first. The existence of ~989 relationship memories does not prove equivalent facts exist.
- **0.4.0:** leave unyanked. Replace “yank if someone installs it accidentally” with proactive triggers: known corruption, security issue, or incompatible migration behavior.
- **Purge:** implementation may be deferred, but only with an explicit product warning that `forget`/prune are not erase, a risk owner, and a reopening trigger. Secrets remain in memories, chunks and journal payloads today ([HANDOFF.md:150–154](/Users/dev/Projects/engram-cli/docs/arch/HANDOFF.md:150)).

**Verdict: NO-GO with revised order: A in background + rebase the subtraction plan → expanded D and E rulings → C identity/bootstrap/status → atomic B → subtraction after every gate-4 prerequisite → safe C sync/prune → combined B×C gate → post-subtraction score calibration.**
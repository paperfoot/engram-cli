## Verdict

The lane correctly targets the real move-4 defects (`facts.rs:451–517`, `serve.rs:42–46`/`36`/`129`, `compile.rs:186–187`, recall conflicts gap) and is unusually careful on `completed` vs `MEMORY_CONSTRAINS_DERIVED`. It is **not implementable as written**: the storage jobs module cannot compile against today’s privacy rules, legacy `create_compile_job` collides with the new unique index, `--store` + `agent-info` contradicts CC-7, and several journal/respond contracts the peer specs already pinned are missing or reinvented.

Pin claimed: `81e6a87`. Tree reviewed: `a239e0e` (ancestor; cited line anchors still hold for `serve`/`facts`/`compile`/`sqlite`).

---

### Critical — hard constraints / interface / peer-spec collisions

**1. `jobs.rs` cannot access `SqliteStore::conn` (false Rust claim; ownership exception under-specified)**  
§4.2 / §9.1: “an `impl SqliteStore` block in a sibling module can reach the private `conn` field.”  
`conn` is private in `sqlite.rs:25–26`. Sibling modules cannot see it.  
`spec-storage-journal.md` plans `pub(crate) conn` for *its* modules; this spec’s §10.1 does **not** request that visibility change. As written, `crates/engram-storage/src/jobs.rs` does not compile, and owning any file under `engram-storage/src/**` also violates the brief ownership table unless the lead grants the exception. Implementer must re-derive: submodule of `sqlite`, `pub(crate) conn` patch, or storage-journal implements §4.2.

**2. `idx_jobs_live` breaks the preserved legacy compile path**  
§4.1 adds `UNIQUE … WHERE status IN ('queued','running')` while §4.2 / `compile.rs:116` keep `create_compile_job` (`sqlite.rs:2621–2646`), which only abandons `status='running'` then `INSERT … 'running'`.  
A live `queued` row from `jobs submit` → sync `engram compile` hits a unique-constraint error that `create_compile_job` does not translate. §8.1’s “downgrade-safe / old binary still creates jobs” analysis only covers abandon-then-insert against itself, not queued↔running coexistence in the **same** wave-4 binary. Hard compatibility hole inside the lane.

**3. `--store` creates directories before `agent-info` (CC-7 / R14 / brief constraint 1 adjacent)**  
§8.3: `apply_store_override` runs as the **first** statement of `dispatch`, “creating the directory if needed,” *and* claims `engram --store X agent-info` “still opens nothing” / creates no directories via peek. Those cannot both be true. Landed behaviour (`lib.rs:22–27`, `paths::*_peek`) is that discovery must not mutate the filesystem. This reintroduces side effects on the exact surface CC-7 marked fixed.

**4. Invented `respond.rs` API vs contract-codegen / R12**  
§3.8 / §10.2 require `respond::{success,error,listing,conflicts,resolve_block}`.  
`spec-contract-codegen.md` §2.4 pins `resolve_conflict` / `attach` / `resolve_command_string` — no envelope builders, no `conflicts()`, no `listing()`. Daemon-jobs either ships a fourth private envelope layer (exactly what CM-9 / R12 forbade) or silently redesigns contract-codegen’s module. Also duplicates `ResolveAction` (codegen `respond.rs` vs this `engine.rs` §5.2).

**5. Journal obligations assigned to this lane are absent**  
`spec-storage-journal.md` §2.3: **`compile.ran` is appended by daemon-jobs in wave 4**. Interfaces §2 also define `job.submitted` / `job.completed` / `job.failed`.  
This spec’s apply/finish/submit paths never append any of them. Rebuild ignores those kinds for projection, but invariant 1 / export-v3 completeness still requires the events. Implementer must invent when/where they fire.

**6. §7.5 `write_fact` fallback breaks same-transaction invariant**  
Fallback: `block_on(write_fact(...))` inside `Core::write` with `embedder: None`.  
`write_fact` (`fact_write.rs:858+`) mutates via `ctx.store`, not `WriteCtx.store`. `AppContext.store` and the writer thread’s connection are different connections (§2.6 keeps both). Facts would commit on the CLI connection **outside** the compile apply transaction — rollback of `apply_compile_plan` would not roll them back. The “fallback if applier not exposed” is unsafe, not merely ugly.

**7. `ConflictAlreadyResolved` misused for job cancel (R11)**  
§4.7: `POST /v1/jobs/{id}/cancel` on an already-terminal job → `409 conflict_already_resolved`.  
R11 defines that code for the facts-resolve claim race. Agents branching on `error.code` will conflate adjudication with job lifecycle. Needs a distinct code or `bad_input`/`not_found`-class error.

---

### Hard-constraint / live-store / compatibility

**8. Dual writers until wave 5/6 — residual risk understated for release builds**  
§1.2.2 / §7.3.1 / §12.1: transitional `ctx.store` writes + `Core` writer, guarded mainly by `debug_assert!(!in_transaction)`. Release builds have no assert; nested `Core::write` from a `with_tx` is a 5s stall then busy. Brief move 4’s “single-writer core” is deferred without a release-time structural guard.

**9. Move 4 “thin clients” narrowed without a binding ruling**  
Brief: CLI embeds core **when no daemon**. §1.2.1: CLI **always** embeds; daemon detection only for job scheduling. Open Q6 admits a future `RemoteCore`. Until ruled, wave 4 does not deliver the move-4 client model the brief describes — only half of it.

**10. `GET /v1/jobs/{id}` `200 {job:null}` → `404`**  
§4.7 / §8.2 / Q5: deliberate break. Interfaces §1 supports `not_found`; constraint 3 (“existing CLI surface keeps working”) does not cleanly cover HTTP clients that treat 200+null as “missing.” Needs an explicit lead sanction (spec asks; not yet granted in interfaces v2).

**11. Stale tree pin + ErrorCode already partially landed**  
Spec pins `81e6a87`; HEAD has `ErrorCode` in `engram-core` (`a239e0e`) while `CliError` (`error.rs`) is still pre-R11 (`Storage → exit 1`, codes like `config_error` / `serde_error`). §3.4’s `http_status` assumes post-codegen `code()` strings (`"config"`, `"malformed_json"`). Implementer must track which wave-2 pieces actually exist vs this doc’s assumptions.

---

### Migration / schema holes

**12. `ensure_jobs_schema` vs storage-journal v10 — collision not contracted**  
§4.1 / §8.1: no version bump; lazy `ALTER`s; “if storage-journal folds into v10, both may run.”  
Storage-journal owns `migrate_v10` and `CURRENT_VERSION` 9→10, and already lists `compile_jobs` among rebuild-touched tables. No shared DDL list for the new job columns. Risk: duplicate index names, ordering fights, or wave-4 ALTERs on a store mid-migration. Open Q2–Q3 are real blockers, not polish.

**13. `compile_pressure` / sleep trigger vs job `kind` default**  
Pressure keys off newest `status='completed'` compile. After §4.1, `kind` defaults to `'compile'` for legacy rows — OK — but pressure SQL must ignore `backfill`/`reflect` completions. §4.2 sketch does not say `AND kind='compile'`. Easy to count the wrong job and suppress or spuriously trigger sleep compile.

---

### Missing failure modes / implementer must re-derive

**14. `JobCtx` embeds `AppContext` — connection identity unspecified**  
§4.3 / §7.2 `for_job`: “same store handle.” `SqliteStore` is not `Clone` (owns `Connection`). If the handle is the writer connection, async runner + `core.write` deadlocks or aliases. If it is a second connection, command code that still writes via `ctx.store` bypasses `Core`. No normative rule.

**15. `may_close` in prose, not in the resolve sequence**  
§5.4 says refuse when `may_close` returns `Refuse`; §5.2 steps 1–8 never call it. Where it runs relative to claim (step 2) changes whether a refused safety close leaves the conflict open (rollback) — intended — but the call site and `CloseAuthority` construction (`Caller { actor }` vs string `"caller"`) are left to the implementer.

**16. `ResolveAction` → `DecisionVerdict` mapping missing (R4)**  
Interfaces: verdict `"keep_both"|"close_old"|"reject_new"|"void"`. CLI/actions: `supersede-old|keep-both|reject-new`. §5.2 step 6 passes `verdict` into `record_decision` with no map (`SupersedeOld` → `close_old`, etc.). Wrong strings break replay/lookup.

**17. `record_decision` signature invented vs frozen `DecisionRow` (R8)**  
§5.2 lists a long positional call; interfaces §3.4 freezes `DecisionRow` and an underspecified `record_decision`. Pair with fact-semantics/storage signatures before coding or the resolve body won’t typecheck across waves.

**18. SSE holds a connection permit indefinitely**  
§12.1 risk 6 notes `ENGRAM_JOBS_MAX_SUBSCRIBERS` / 1h timeout — those limits never appear in §3.2 `Limits` or §4.8 normative text. Easy to ship without them.

**19. Auth constant-time compare still length-leaks by early return on length mismatch**  
§3.3.8: length compared separately, then XOR. Correct for equal-length tokens; still distinguishable timing on length. Minor vs today’s `ends_with`, but the “constant-time” claim oversells.

**20. Reader pool “migrated once” is false as specified**  
§3.2 / §2.2: `warm_readers(n)` opens `n` × `SqliteStore::open`, and `open` always calls `migrate()` (`sqlite.rs:812`). That is n migrate attempts at startup, not one. Harmless if idempotent; the defect narrative (“delete per-request migrate”) is still right.

**21. `surface_conflicts_for` alone does not satisfy R12**  
§3.8 / §10.3 only reparameterize `surface_conflicts` (`recall.rs:296–353`), which still emits a piped `resolve_command` (`:345–348`). Without also routing contested rows through `respond::attach` / codegen’s builder, `/v1/recall` keeps the IN-24 pipe bug the lane claims to close.

**22. Tests gated on wave-5 `cfg` for core promises**  
§11.9: `keep_both_records_a_decision` (CD-3 precondition this lane owns in §5.2) and respond-dependent tests sit behind a feature “wave-5 turns on.” Wave 4 can go green without proving the resolve/journal behaviour it specifies — same CC-4 pattern the coherence report rejected.

**23. Ingest subprocess + dual process writers**  
§3.5 keeps `/v1/ingest` shell-out; §12.1.8 notes backfill subprocess fallback reintroduces a second writer. Under §7.3.1’s transitional dual-writer story, ingest is another unsynchronized writer process forever in wave 4. Not a new defect vs today, but it undercuts “single-writer Core” for the daemon’s own mutating surface.

---

### What is solid (so the lead doesn’t re-litigate)

- Defect anchors for HTTP framing, serial accept, per-request `open`/`migrate`, non-transactional resolve discarding claim bools, and compile lock-across-LLM match the code.  
- `completed` must mean full derived rebuild — correctly tied to `MEMORY_CONSTRAINS_DERIVED` (`sqlite.rs:782–789`) with a dedicated test.  
- Claim-before-mutate resolve order and cycle guard address the brief’s two-success-cycle failure mode.  
- Sleep-compile `llm: false` by default is the right safety default.  
- Cross-lane patch channel usage (§10) matches R15 in form; several patch *contents* are wrong or incomplete (above).

---

### Highest-priority lead rulings needed before implementation

1. §9.1 ownership of `engram-storage/src/jobs.rs` **and** `pub(crate) conn` (or force storage-journal to land §4.2).  
2. How legacy `create_compile_job` coexists with `idx_jobs_live` (abandon queued too? route sync compile through `submit_job`? drop the unique index?).  
3. `--store` must not create directories on discovery paths.  
4. Who owns HTTP envelope helpers — extend contract-codegen `respond.rs`, or daemon-only wrappers calling R12’s attach API.  
5. Normative append points for `compile.ran` / `job.*`.  
6. Sanction (or reject) the jobs `404` behaviour change.

---

## Addendum — findings verified against the tree during triage (not in the Grok pass)

**A1. A panicking write closure leaves the writer connection inside an open transaction.**
§2.4 wraps `task(&store)` in `catch_unwind` and claims "the transaction is rolled back by
`with_tx_res`'s guard", but §10.1 requests `with_tx_res` as "the existing body" and `with_tx`
(`sqlite.rs:831-850`) has no unwind guard: it matches on `Ok`/`Err` and never rolls back on a
panic. After one panic `conn.is_autocommit()` is false forever, and `with_tx`'s
join-the-open-transaction branch (`:838-840`) means every later write silently joins a
transaction that is never committed. The writer thread must issue an explicit `ROLLBACK` after
`catch_unwind` catches, or `with_tx_res` must roll back on unwind.

**A2. `ensure_jobs_schema()` never runs on a read-only process, so `jobs list` breaks on a v9 store.**
§4.1 says it is "called once from `Core::new`'s first store touch", but §2.3 makes `Core::new`
path-only and the writer lazy (§11.2 `writer_thread_is_lazy` pins that). `engram jobs list` reads
through `ctx.store` and would `SELECT kind, params_json, …` against a table that has never been
ALTERed. It must run at store-open time (or in storage-journal's v10 step).

**A3. `CREATE UNIQUE INDEX idx_jobs_live` can fail outright on an existing store.**
`create_compile_job` (`sqlite.rs:2621-2646`) abandons-then-inserts in two unguarded statements, so
two concurrent 0.3.x compiles of one KB can leave two `running` rows. `CREATE UNIQUE INDEX … IF NOT
EXISTS` errors on pre-existing duplicates; the failure lands inside schema setup and bricks every
command. interfaces §2.1 step 5 already sets the precedent (collapse duplicate keys first, create
the unique index LAST); this spec skips the collapse.

**A4. The resolve path appends no `fact.interval_closed`, so `admin rebuild` cannot reproduce it (R2).**
§5.2 steps 5-7 mutate `superseded_by` via `supersede_fact_with` and append only `decision.recorded`.
interfaces §2 R2 requires `closed_by`/`close_reason`/`close_authority` and the `superseded_by`
compat write to be rebuildable from `fact.interval_closed`, and invariant 3 requires a byte-identical
projection rebuild. A store where a conflict was resolved would not survive `admin rebuild`.

**A5. `DecisionRow.event_id` forces event-before-decision ordering.**
interfaces §3.4 freezes `DecisionRow` with `event_id: Uuid`; §5.2 records the decision at step 6 and
appends the event at step 7, with no event id to hand. Order must invert (or `record_decision` takes
the draft and appends internally).

**A6. The sleep-time idle gate cannot see CLI writes.**
§6.1's idle signal is `CoreMetrics.last_write_at`, which is per-process. Every non-HTTP mutation
happens in a *different* process (the CLI's own `Core`), so the daemon considers the store idle
while an agent is hammering it through `engram remember`. The idle signal has to be store-derived
(newest event/memory timestamp) to mean what §6.1 claims.

**A7. Reader pool design is unspecified and cannot be `&SqliteStore` across tasks.**
`Connection` is `Send` but not `Sync`, so `Core::read`'s `spawn_blocking` closure cannot borrow a
shared `&SqliteStore`; the pool needs owned/`Mutex`-guarded connections checked out per call. Also
`SqliteStore::open` always calls `migrate()` (`sqlite.rs:812`), so `warm_readers(8)` runs eight
migrations at startup — which contradicts §11.2's `no_reader_migration_per_request` narrative and
needs an open-without-migrate path.

**A8. `body_over_limit_is_413` is likely to be a flaky test as written.**
Answering 413 without reading the body and closing immediately usually sends an RST while the
client is still writing, which destroys the already-queued response on most stacks. The server needs
a bounded drain (or a half-close + linger) before closing, and the test needs to state which.
